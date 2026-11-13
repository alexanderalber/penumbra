//! Rigid bodies: a [`World`] of groups of bodies that fall, collide and come to rest, on a floor
//! and on triangle meshes. A group is copies of one [`Shape`] with one [`Material`]; bodies of
//! every group collide with each other. A group's [`Mode`] says who moves its bodies: the
//! solver, or the caller, whose bodies then push the solver's out of their way. Obstacles may
//! be moved too and carry along what they touch. Space is penumbra's: metres, z up. Rotations
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

/// Who moves the bodies of a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The solver: the bodies fall, collide and come to rest.
    #[default]
    Dynamic,
    /// The caller, through [`World::place`]: a body travels to where it is placed within the
    /// next step and pushes dynamic bodies out of its way. Nothing pushes it back.
    Driven,
    /// Nobody: the bodies stand where they are placed and others land on them.
    /// [`World::place`] puts them elsewhere at once.
    Fixed,
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

/// Where something stands and how it is turned, without a change of size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub translate: [f32; 3],
    /// The rows of a rotation matrix.
    pub turn: [[f32; 3]; 3],
}

impl Default for Place {
    fn default() -> Self {
        Self { translate: [0.0; 3], turn: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] }
    }
}

/// A triangle mesh that bodies collide with. Its vertices are in its own space, sized as they
/// collide; `place` puts the mesh into the world. [`World::move_obstacles`] gives it another
/// place, and on its way there it pushes bodies along.
pub struct Obstacle {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub place: Place,
}

/// One body of a group.
struct Body {
    body: RigidBodyHandle,
    collider: ColliderHandle,
    scale: [f32; 3],
    /// Where a driven body is to be once the next step is over.
    target: Option<Pose>,
}

/// Copies of one shape with one material.
struct Group {
    shape: Shape,
    material: Material,
    mode: Mode,
    bodies: Vec<Body>,
    /// The hull per scale, since a body's scale is part of its collision shape.
    hulls: Vec<([f32; 3], SharedShape)>,
}

/// Most hulls a group keeps for scales it has seen; a driven body may change its scale on
/// every step.
const MAX_HULLS: usize = 64;

/// An obstacle in the world: a body the caller moves, with the mesh on it.
struct Fixture {
    body: RigidBodyHandle,
    collider: ColliderHandle,
    /// Where it is to be once the next step is over.
    target: Option<Pose>,
}

pub struct World {
    world: PhysicsWorld,
    settings: Settings,
    /// By the group's number; a number without a group holds none.
    groups: Vec<Option<Group>>,
    /// Bodies of all groups together.
    count: usize,
    floor: Option<ColliderHandle>,
    /// In the order they were given; none for an obstacle without triangles.
    obstacles: Vec<Option<Fixture>>,
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

    /// Who moves the bodies of `group`; none for a group that does not exist.
    pub fn mode(&self, group: usize) -> Option<Mode> {
        self.group(group).map(|g| g.mode)
    }

    /// Makes `group` an empty group of bodies that collide as `shape`, are made of `material`
    /// and are moved as `mode` says. Bodies the group held leave the world. An error leaves the
    /// world as it was.
    pub fn set_group(
        &mut self,
        group: usize,
        shape: Shape,
        material: Material,
        mode: Mode,
    ) -> Result<(), String> {
        if group >= MAX_GROUPS {
            return Err(format!("a world holds {MAX_GROUPS} groups at most"));
        }
        let shape = check(shape)?;
        self.remove_group(group);
        if self.groups.len() <= group {
            self.groups.resize_with(group + 1, || None);
        }
        if let Some(slot) = self.groups.get_mut(group) {
            *slot = Some(Group {
                shape,
                material: material.checked(),
                mode,
                bodies: Vec::new(),
                hulls: Vec::new(),
            });
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

    /// Adds a body to `group`. A body of a group that is not dynamic takes its place, turn and
    /// scale from `start` and nothing else. An error leaves the world as it was.
    pub fn add(&mut self, group: usize, start: &Start) -> Result<(), String> {
        if self.count >= MAX_BODIES {
            return Err(format!("more than {MAX_BODIES} bodies"));
        }
        if !finite(start) {
            return Err("a body's numbers must be finite".into());
        }
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else {
            return Err(format!("there is no group {group}"));
        };
        let scale = sized(start.scale);
        let collider = g.collider(scale)?;
        let body = match g.mode {
            Mode::Dynamic => RigidBodyBuilder::dynamic()
                .linvel(vector(start.velocity))
                .angvel(vector(start.spin.map(f32::to_radians)))
                .linear_damping(self.settings.damping)
                .angular_damping(self.settings.spin_damping),
            Mode::Driven => RigidBodyBuilder::kinematic_position_based(),
            Mode::Fixed => RigidBodyBuilder::fixed(),
        };
        let (body, collider) = self.world.insert(body.pose(pose(start)), collider);
        g.bodies.push(Body { body, collider, scale, target: None });
        self.count += 1;
        Ok(())
    }

    /// Says where the bodies of a driven or fixed `group` are to be: body k goes to row k, with
    /// the row's place, turn and scale. Rows beyond the bodies add bodies, bodies beyond the
    /// rows leave the world.
    ///
    /// A driven body travels to its row within the next [`World::step`] and pushes what is in
    /// its way with the speed that takes. A row farther than `jump` metres from its body puts
    /// the body there at once instead, and nothing is pushed. A fixed body is always put there
    /// at once. A change of scale never pushes: the body is the new size at once.
    ///
    /// An error for a dynamic group, for rows that are not finite (the world stays as it was)
    /// and for rows the world has no room for (the rows before them are placed).
    pub fn place(&mut self, group: usize, rows: &[Start], jump: f32) -> Result<(), String> {
        if !rows.iter().all(finite) {
            return Err("a body's numbers must be finite".into());
        }
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else {
            return Err(format!("there is no group {group}"));
        };
        if g.mode == Mode::Dynamic {
            return Err(format!("the solver moves the bodies of group {group}"));
        }
        let mut jumped = false;
        for gone in g.bodies.drain(rows.len().min(g.bodies.len())..) {
            self.world.remove_body(gone.body);
            self.count = self.count.saturating_sub(1);
            jumped = true;
        }
        let mode = g.mode;
        for k in 0..g.bodies.len().min(rows.len()) {
            let (Some(row), Some(scale)) = (rows.get(k), g.bodies.get(k).map(|b| b.scale)) else { break };
            let wanted = sized(row.scale);
            // A shape that cannot be made at the new scale leaves the body the size it is.
            let resized = if wanted == scale { None } else { g.collider(wanted).ok() };
            let Some(body) = g.bodies.get_mut(k) else { break };
            if let Some(collider) = resized {
                self.world.remove_collider(body.collider);
                body.collider = self.world.insert_collider(collider, Some(body.body));
                body.scale = wanted;
                jumped = true;
            }
            let to = pose(row);
            jumped |= match mode {
                Mode::Driven => aim(&mut self.world, body.body, &mut body.target, to, jump),
                _ => put(&mut self.world, body.body, to),
            };
        }
        let have = g.bodies.len();
        if jumped {
            // What rested on a body that left, shrank or was put elsewhere must fall on.
            self.world.wake_up_all(true);
        }
        rows.iter().skip(have).try_for_each(|row| self.add(group, row))
    }

    /// Replaces every obstacle. An error leaves the world without obstacles.
    pub fn set_obstacles(&mut self, obstacles: Vec<Obstacle>) -> Result<(), String> {
        for old in self.obstacles.drain(..).flatten() {
            self.world.remove_body(old.body);
        }
        self.world.wake_up_all(true);
        let triangles: usize = obstacles.iter().map(|o| o.triangles.len()).sum();
        if triangles > MAX_TRIANGLES {
            return Err(format!("the obstacles have {triangles} triangles, at most {MAX_TRIANGLES} work"));
        }
        let failed = |world: &mut Self, why: String| {
            for old in world.obstacles.drain(..).flatten() {
                world.world.remove_body(old.body);
            }
            Err(why)
        };
        for o in obstacles {
            if o.triangles.is_empty() {
                self.obstacles.push(None);
                continue;
            }
            let Some(place) = pose_of(&o.place) else {
                return failed(self, "an obstacle stands at a place that is not a number".into());
            };
            if o.vertices.iter().flatten().any(|v| !v.is_finite()) {
                return failed(self, "an obstacle has a vertex that is not a number".into());
            }
            let count = o.vertices.len();
            if o.triangles.iter().flatten().any(|&i| i as usize >= count) {
                return failed(
                    self,
                    "an obstacle has a triangle that names a vertex it does not have".into(),
                );
            }
            let vertices: Vec<Vector> = o.vertices.into_iter().map(vector).collect();
            // Fixing inner edges keeps a body that slides over a flat mesh from catching on
            // the edges between its triangles. A mesh it cannot work on collides as it is.
            let fixed = ColliderBuilder::trimesh_with_flags(
                vertices.clone(),
                o.triangles.clone(),
                TriMeshFlags::FIX_INTERNAL_EDGES,
            );
            let collider = match fixed.or_else(|_| ColliderBuilder::trimesh(vertices, o.triangles)) {
                Ok(c) => self.ground(c),
                Err(e) => return failed(self, format!("an obstacle cannot be collided with: {e}")),
            };
            // A body the caller moves, so that an obstacle given another place pushes what it
            // meets on the way. While it stands it costs what a mesh without a body costs.
            let body = RigidBodyBuilder::kinematic_position_based().pose(place);
            let (body, collider) = self.world.insert(body, collider);
            self.obstacles.push(Some(Fixture { body, collider, target: None }));
        }
        Ok(())
    }

    /// Gives every obstacle another place, in the order [`World::set_obstacles`] took them. An
    /// obstacle travels there within the next [`World::step`] and pushes the bodies in its way.
    /// One whose place is farther than `jump` metres from where it stands is put there at once
    /// and pushes nothing. An error, and no obstacle moves, when the count differs or a place
    /// is not finite.
    pub fn move_obstacles(&mut self, places: &[Place], jump: f32) -> Result<(), String> {
        if places.len() != self.obstacles.len() {
            return Err(format!("{} places for {} obstacles", places.len(), self.obstacles.len()));
        }
        let Some(poses) = places.iter().map(pose_of).collect::<Option<Vec<Pose>>>() else {
            return Err("an obstacle is to stand at a place that is not a number".into());
        };
        let mut jumped = false;
        for (fixture, to) in self.obstacles.iter_mut().zip(poses) {
            if let Some(f) = fixture {
                jumped |= aim(&mut self.world, f.body, &mut f.target, to, jump);
            }
        }
        if jumped {
            self.world.wake_up_all(true);
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
            let obstacles = self.obstacles.iter().flatten().map(|f| f.collider);
            for handle in self.floor.into_iter().chain(obstacles) {
                if let Some(collider) = self.world.colliders.get_mut(handle) {
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
    /// bodies steadier). Driven bodies and moved obstacles travel to their places evenly over
    /// the whole time, so the speed they push with does not depend on `substeps`.
    ///
    /// For a time that is not positive and finite no body moves on its own. Driven bodies and
    /// moved obstacles are then put at their places at once and push nothing: without time
    /// there is no speed to push with.
    pub fn step(&mut self, seconds: f32, substeps: u32) {
        let mut moves: Vec<(RigidBodyHandle, Pose, Pose)> = Vec::new();
        let bodies = self.groups.iter_mut().flatten().flat_map(|g| &mut g.bodies);
        let driven = bodies.map(|b| (b.body, &mut b.target));
        let moved = self.obstacles.iter_mut().flatten().map(|f| (f.body, &mut f.target));
        for (handle, target) in driven.chain(moved) {
            let Some(to) = target.take() else { continue };
            if let Some(from) = self.world.bodies.get(handle).map(|b| *b.position()) {
                moves.push((handle, from, to));
            }
        }
        if !seconds.is_finite() || seconds <= 0.0 {
            let mut jumped = false;
            for (handle, _, to) in moves {
                jumped |= put(&mut self.world, handle, to);
            }
            if jumped {
                self.world.wake_up_all(true);
            }
            return;
        }
        let n = substeps.clamp(1, MAX_SUBSTEPS);
        self.world.integration_parameters.dt = seconds / n as f32;
        for k in 1..=n {
            // The solver takes a body to its next place within one of its steps, at the speed
            // that takes. Each substep therefore gets its share of the way.
            for (handle, from, to) in &moves {
                let next = if k == n { *to } else { from.lerp(to, k as f32 / n as f32) };
                if let Some(body) = self.world.bodies.get_mut(*handle) {
                    body.set_next_kinematic_position(next);
                }
            }
            self.world.step();
        }
        // The solver wakes a sleeping body only when what moves already touched it. A body
        // that moved fast may have arrived inside one that sleeps, without a touch before.
        let mut reached: Vec<RigidBodyHandle> = Vec::new();
        for (handle, _, _) in &moves {
            let colliders = self.world.bodies.get(*handle).map(RigidBody::colliders).unwrap_or_default();
            for aabb in
                colliders.iter().filter_map(|c| self.world.colliders.get(*c)).map(Collider::compute_aabb)
            {
                let near = self.world.intersect_aabb_conservative(aabb, QueryFilter::only_dynamic());
                reached.extend(near.filter_map(|(_, collider)| collider.parent()));
            }
        }
        for handle in reached {
            self.world.wake_up(handle, true);
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
    /// What a body of this group collides with at `scale`: its shape, of the group's material.
    fn collider(&mut self, scale: [f32; 3]) -> Result<ColliderBuilder, String> {
        Ok(self
            .shape_at(scale)?
            .friction(self.material.friction)
            .restitution(self.material.bounce)
            .restitution_combine_rule(CoefficientCombineRule::Max)
            .density(self.material.density))
    }

    /// The collision shape at `scale`.
    fn shape_at(&mut self, scale: [f32; 3]) -> Result<ColliderBuilder, String> {
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
                if self.hulls.len() >= MAX_HULLS {
                    self.hulls.clear();
                }
                self.hulls.push((scale, shape.clone()));
                Ok(ColliderBuilder::new(shape))
            }
        }
    }
}

/// Whether every number of `start` is finite.
fn finite(start: &Start) -> bool {
    let all = [start.translate, start.rotate_degrees, start.scale, start.velocity, start.spin];
    all.iter().flatten().all(|v| v.is_finite())
}

/// A scale as a body takes it: positive and away from zero.
fn sized(scale: [f32; 3]) -> [f32; 3] {
    scale.map(|s| s.abs().max(MIN_SCALE))
}

/// Where `start` places a body.
fn pose(start: &Start) -> Pose {
    Pose::from_parts(vector(start.translate), rotation(start.rotate_degrees))
}

/// `place` as the solver takes it; none when a number of it is not finite.
fn pose_of(place: &Place) -> Option<Pose> {
    let [x, y, z] = place.turn;
    let turn = Rotation::from_mat3(&Matrix::from_cols(vector(x), vector(y), vector(z)).transpose());
    let pose = Pose::from_parts(vector(place.translate), turn.normalize());
    pose.is_finite().then_some(pose)
}

/// Puts the body `handle` at `to` at once, without a speed. Returns whether it stood elsewhere.
fn put(world: &mut PhysicsWorld, handle: RigidBodyHandle, to: Pose) -> bool {
    let Some(body) = world.bodies.get_mut(handle) else { return false };
    if *body.position() == to && *body.next_position() == to {
        return false;
    }
    body.set_position(to, true);
    true
}

/// Sends the body `handle`, which the caller moves, to `to`: it is there after the next step,
/// by way of `target`. Farther than `jump` metres from where it stands it is put there at once
/// instead. Returns whether it was.
fn aim(
    world: &mut PhysicsWorld,
    handle: RigidBodyHandle,
    target: &mut Option<Pose>,
    to: Pose,
    jump: f32,
) -> bool {
    let Some(from) = world.bodies.get(handle).map(|b| *b.position()) else { return false };
    // Not a number counts as no way at all, so a bad `jump` never flings a body.
    let near = (to.translation - from.translation).length() <= jump;
    if near {
        *target = (from != to).then_some(to);
        false
    } else {
        *target = None;
        put(world, handle, to)
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
        world.set_group(0, shape, Material::default(), Mode::Dynamic).unwrap();
        world
    }

    #[test]
    fn a_ball_and_a_box_of_two_groups_collide() {
        let mut world = World::new(Settings::default());
        world.set_group(0, unit_box(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(3, ball(), Material::default(), Mode::Dynamic).unwrap();
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
        world.set_group(3, unit_box(), Material::default(), Mode::Dynamic).unwrap();
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
                place: Place::default(),
            }])
            .unwrap();
        world.set_group(0, unit_box(), material, Mode::Dynamic).unwrap();
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
            world
                .set_group(0, ball(), Material { bounce: ball_bounce, ..Material::default() }, Mode::Dynamic)
                .unwrap();
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
            world
                .set_group(0, ball(), Material { density: left, ..Material::default() }, Mode::Dynamic)
                .unwrap();
            world
                .set_group(1, ball(), Material { density: right, ..Material::default() }, Mode::Dynamic)
                .unwrap();
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
        world.set_group(0, ball(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(1, ball(), Material::default(), Mode::Dynamic).unwrap();
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
            place: Place::default(),
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
        let bad_index =
            Obstacle { vertices: vec![[0.0; 3]; 3], triangles: vec![[0, 1, 7]], place: Place::default() };
        assert!(world.set_obstacles(vec![bad_index]).is_err());
        let bad_vertex = Obstacle {
            vertices: vec![[f32::NAN; 3]; 3],
            triangles: vec![[0, 1, 2]],
            place: Place::default(),
        };
        assert!(world.set_obstacles(vec![bad_vertex]).is_err());
        assert!(check(Shape::Hull { points: vec![[0.0; 3]; 8] }).is_err());
        assert!(check(Shape::Hull { points: vec![[1.0, 0.0, 0.0]] }).is_err());
        assert!(check(Shape::Sphere { center: [0.0; 3], radius: f32::NAN }).is_err());
        // A shape that fails leaves the group as it was; so does a group beyond the last.
        world.add(0, &at(1.0)).unwrap();
        assert!(
            world
                .set_group(0, Shape::Hull { points: vec![[0.0; 3]; 8] }, Material::default(), Mode::Dynamic)
                .is_err()
        );
        assert_eq!(world.group_len(0), 1);
        assert!(world.set_group(MAX_GROUPS, unit_box(), Material::default(), Mode::Dynamic).is_err());
        assert!(world.add(MAX_GROUPS, &at(1.0)).is_err());
        assert!(world.add(7, &at(1.0)).is_err(), "no such group");
        world.set_material(7, Material::default());
        world.remove_group(usize::MAX);
        assert_eq!((world.len(), world.placements(7).len()), (1, 0));
        // Settings and materials that are not numbers fall back instead of failing.
        let odd = Settings { gravity: [f32::NAN; 3], friction: -3.0, bounce: 9.0, ..Settings::default() };
        let mut world = World::new(odd);
        let strange = Material { friction: f32::NAN, bounce: -1.0, density: 0.0 };
        world.set_group(0, unit_box(), strange, Mode::Dynamic).unwrap();
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
        world.set_group(1, ball(), Material::default(), Mode::Dynamic).unwrap();
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

    fn at_x(x: f32, z: f32) -> Start {
        Start { translate: [x, 0.0, z], ..Start::default() }
    }

    /// A world with a dynamic box of 1 m at rest (and asleep) on the floor at the origin as
    /// group 0, and an empty group 1 of boxes moved as `mode` says.
    fn box_at_rest_and(mode: Mode) -> World {
        let mut world = world_of(unit_box(), Settings::default());
        world.set_group(1, unit_box(), Material::default(), mode).unwrap();
        world.add(0, &at(0.5)).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.asleep(), 1);
        world
    }

    const NEVER: f32 = f32::INFINITY;

    /// A driven box sweeps along x from -3 to 3 in one second, through the box at rest.
    /// Returns where the box at rest was when the sweep ended, where the driven one was, and
    /// the fastest the pushed box went in x, in metres per second.
    fn sweep(substeps: u32) -> (f64, [f64; 3], f64) {
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-3.0, 0.5)).unwrap();
        let (mut fastest, mut last) = (0.0f64, 0.0f64);
        for k in 1..=60 {
            world.place(1, &[at_x(-3.0 + 0.1 * k as f32, 0.5)], NEVER).unwrap();
            world.step(DT, substeps);
            let x = world.placements(0)[0].translate[0];
            fastest = fastest.max((x - last) / f64::from(DT));
            last = x;
        }
        (last, world.placements(1)[0].translate, fastest)
    }

    #[test]
    fn a_driven_body_pushes_a_resting_one_away_and_is_not_pushed_back() {
        let (pushed, driven, fastest) = sweep(1);
        // The driven box is where its last row put it, to the digit: nothing held it up.
        assert!((driven[0] - 3.0).abs() < 1e-4 && (driven[2] - 0.5).abs() < 1e-6, "{driven:?}");
        // The other box was woken and pushed ahead of it: its near side is at least at the
        // driven box's far side.
        assert!(pushed > 3.9, "the box at rest ended at {pushed}");
        // The driven box moved at 6 m/s. What it pushes goes about as fast, with one solver
        // step per frame and with four: the speed does not grow with the substeps.
        assert!(fastest > 4.0 && fastest < 12.0, "pushed at {fastest} m/s");
        let (pushed4, driven4, fastest4) = sweep(4);
        assert!((driven4[0] - 3.0).abs() < 1e-4, "{driven4:?}");
        assert!(pushed4 > 3.9 && fastest4 > 4.0 && fastest4 < 12.0, "{pushed4} at {fastest4} m/s");
    }

    #[test]
    fn a_body_on_a_driven_platform_rides_along() {
        let mut world = World::new(Settings { floor: None, ..Settings::default() });
        let slab = Shape::Box { center: [0.0; 3], half: [2.0, 2.0, 0.1] };
        world.set_group(0, unit_box(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(1, slab, Material::default(), Mode::Driven).unwrap();
        world.add(1, &at(0.0)).unwrap();
        world.add(0, &at(0.6)).unwrap();
        run(&mut world, 2.0);
        assert!((world.placements(0)[0].translate[2] - 0.6).abs() < 0.02, "it rests on the platform");
        // The platform goes 3 m along x in three seconds, then up by 1 m in one.
        for k in 1..=180 {
            world.place(1, &[at_x(k as f32 / 60.0, 0.0)], NEVER).unwrap();
            world.step(DT, 1);
        }
        let rider = world.placements(0)[0].translate;
        assert!((rider[0] - 3.0).abs() < 0.15, "the rider went along: {rider:?}");
        for k in 1..=60 {
            world.place(1, &[at_x(3.0, k as f32 / 60.0)], NEVER).unwrap();
            world.step(DT, 1);
        }
        run(&mut world, 1.0);
        let rider = world.placements(0)[0].translate;
        assert!((rider[2] - 1.6).abs() < 0.03 && (rider[0] - 3.0).abs() < 0.15, "lifted: {rider:?}");
        // A platform that stays where it is holds the rider without a new row.
        let before = world.placements(1);
        run(&mut world, 1.0);
        assert_eq!(before, world.placements(1));
        assert!((world.placements(0)[0].translate[2] - 1.6).abs() < 0.03);
    }

    #[test]
    fn a_fixed_body_holds_what_lands_on_it_and_goes_where_it_is_put() {
        let mut world = World::new(Settings { floor: None, ..Settings::default() });
        world.set_group(0, ball(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(1, unit_box(), Material::default(), Mode::Fixed).unwrap();
        // A speed in the row means nothing to a fixed body.
        world.add(1, &Start { velocity: [5.0, 0.0, 0.0], spin: [90.0; 3], ..Start::default() }).unwrap();
        world.add(0, &at(3.0)).unwrap();
        run(&mut world, 3.0);
        assert_eq!(world.placements(1)[0].translate, [0.0; 3], "gravity does not move it");
        assert!((world.placements(0)[0].translate[2] - 1.0).abs() < 0.02, "the ball rests on it");
        // Put elsewhere it is there at once, and the ball that rested on it falls on.
        world.place(1, &[at_x(10.0, 0.0)], 0.0).unwrap();
        world.step(DT, 1);
        assert_eq!(world.placements(1)[0].translate, [10.0, 0.0, 0.0]);
        run(&mut world, 1.0);
        let ball = world.placements(0)[0].translate;
        assert!(ball[2] < -1.0 && ball[0].abs() < 0.05, "it fell straight down: {ball:?}");
    }

    /// A slab of 4 x 4 m as an obstacle, its top at z = 0 in its own space.
    fn slab(place: Place) -> Obstacle {
        Obstacle {
            vertices: vec![[-2.0, -2.0, 0.0], [2.0, -2.0, 0.0], [2.0, 2.0, 0.0], [-2.0, 2.0, 0.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            place,
        }
    }

    fn placed_at(x: f32, z: f32) -> Place {
        Place { translate: [x, 0.0, z], ..Place::default() }
    }

    #[test]
    fn a_moved_obstacle_carries_what_rests_on_it_and_pushes_what_is_in_its_way() {
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        world.set_obstacles(vec![slab(placed_at(0.0, 1.0))]).unwrap();
        world.add(0, &at(1.6)).unwrap();
        run(&mut world, 2.0);
        assert!((world.placements(0)[0].translate[2] - 1.5).abs() < 0.02);
        assert_eq!(world.asleep(), 1);
        // The slab goes 2 m along x in two seconds and takes the box along; then it rises by
        // 1 m in one second and lifts it.
        for k in 1..=120 {
            world.move_obstacles(&[placed_at(k as f32 / 60.0, 1.0)], NEVER).unwrap();
            world.step(DT, 1);
        }
        let carried = world.placements(0)[0].translate;
        assert!((carried[0] - 2.0).abs() < 0.15, "carried along: {carried:?}");
        for k in 1..=60 {
            world.move_obstacles(&[placed_at(2.0, 1.0 + k as f32 / 60.0)], NEVER).unwrap();
            world.step(DT, 1);
        }
        run(&mut world, 1.0);
        let lifted = world.placements(0)[0].translate;
        assert!((lifted[2] - 2.5).abs() < 0.03, "lifted, it did not pass through: {lifted:?}");
        // A wall turned upright by its place sweeps a box off the floor's height.
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(0.5)).unwrap();
        run(&mut world, 2.0);
        // Turned by 90 degrees about y: the slab's own z looks along x.
        let wall = |x: f32| Place {
            translate: [x, 0.0, 0.0],
            turn: [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
        };
        world.set_obstacles(vec![slab(wall(-2.0))]).unwrap();
        for k in 1..=120 {
            world.move_obstacles(&[wall(-2.0 + k as f32 / 30.0)], NEVER).unwrap();
            world.step(DT, 2);
        }
        let pushed = world.placements(0)[0].translate;
        assert!(pushed[0] > 2.45, "the wall at x = 2 has the box in front of it: {pushed:?}");
    }

    #[test]
    fn a_far_jump_puts_a_body_there_and_pushes_nothing() {
        // The driven box starts left of the box at rest and is placed right of it, 6 m away.
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-3.0, 0.5)).unwrap();
        world.place(1, &[at_x(3.0, 0.5)], 1.0).unwrap();
        world.step(DT, 1);
        run(&mut world, 1.0);
        assert!((world.placements(1)[0].translate[0] - 3.0).abs() < 1e-5);
        let other = world.placements(0)[0].translate;
        assert!(other[0].abs() < 1e-3, "nothing swept the box away: {other:?}");
        // A way within the limit that ends inside the box at rest: the box sleeps and nothing
        // touched it before, yet it is woken and pushed out, by the 0.4 m the two overlap.
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-1.4, 0.5)).unwrap();
        world.place(1, &[at_x(-0.6, 0.5)], 1.0).unwrap();
        world.step(DT, 1);
        run(&mut world, 1.0);
        assert!(world.placements(0)[0].translate[0] > 0.39, "{:?}", world.placements(0)[0]);
        // So it is with obstacles: a slab that jumps away from under a box lets it fall.
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        world.set_obstacles(vec![slab(placed_at(0.0, 0.0))]).unwrap();
        world.add(0, &at(0.5)).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.asleep(), 1);
        world.move_obstacles(&[placed_at(20.0, 0.0)], 1.0).unwrap();
        run(&mut world, 1.0);
        let fell = world.placements(0)[0].translate;
        assert!(fell[2] < -1.0 && fell[0].abs() < 0.05, "{fell:?}");
    }

    #[test]
    fn without_time_a_driven_body_is_put_at_its_row_and_pushes_nothing() {
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-3.0, 0.5)).unwrap();
        // Time stands (a show on hold): the row moves the body all the same, through the box
        // at rest, in steps well under the limit.
        for k in 1..=60 {
            world.place(1, &[at_x(-3.0 + 0.1 * k as f32, 0.5)], 1.0).unwrap();
            world.step(0.0, 1);
        }
        assert!((world.placements(1)[0].translate[0] - 3.0).abs() < 1e-4);
        run(&mut world, 1.0);
        assert!(world.placements(0)[0].translate[0].abs() < 1e-3, "{:?}", world.placements(0)[0]);
    }

    #[test]
    fn bodies_come_and_go_with_the_rows_and_take_their_scale() {
        let mut world = World::new(Settings { gravity: [0.0; 3], floor: None, ..Settings::default() });
        world.set_group(0, unit_box(), Material::default(), Mode::Driven).unwrap();
        world.set_group(1, ball(), Material::default(), Mode::Dynamic).unwrap();
        let rows = |n: usize| -> Vec<Start> { (0..n).map(|k| at_x(k as f32 * 3.0, 0.0)).collect() };
        world.place(0, &rows(3), 1.0).unwrap();
        world.step(DT, 1);
        assert_eq!((world.group_len(0), world.len()), (3, 3));
        assert_eq!(world.placements(0)[2].translate, [6.0, 0.0, 0.0], "a new body is at its row at once");
        // Fewer rows: the last bodies leave. Row k stays body k.
        let mut two = rows(2);
        two[1].translate[1] = 0.25;
        world.place(0, &two, 1.0).unwrap();
        world.step(DT, 1);
        assert_eq!((world.group_len(0), world.len()), (2, 2));
        assert!((world.placements(0)[1].translate[1] - 0.25).abs() < 1e-6);
        world.place(0, &[], 1.0).unwrap();
        assert!(world.is_empty());
        // A row's scale sizes the body while it is driven: a ball 0.2 m beside a box of 1 m
        // is pushed away when the box grows to 3 m.
        world.place(0, &rows(1), 1.0).unwrap();
        world.add(1, &at_x(1.2, 0.0)).unwrap();
        run(&mut world, 0.5);
        assert!((world.placements(1)[0].translate[0] - 1.2).abs() < 1e-3);
        world.place(0, &[Start { scale: [3.0; 3], ..Start::default() }], 1.0).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.placements(0)[0].scale, [3.0; 3]);
        assert!(world.placements(1)[0].translate[0] > 1.95, "{:?}", world.placements(1)[0].translate);
        // The world's limit holds for placed rows too: the rows before it are placed.
        let mut world = World::new(Settings::default());
        world.set_group(0, unit_box(), Material::default(), Mode::Fixed).unwrap();
        let many: Vec<Start> = (0..MAX_BODIES + 5).map(|k| at(k as f32 * 2.0)).collect();
        assert!(world.place(0, &many, 1.0).is_err());
        assert_eq!(world.len(), MAX_BODIES);
    }

    #[test]
    fn bad_places_are_errors_and_move_nothing() {
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-3.0, 0.5)).unwrap();
        assert_eq!(
            (world.mode(0), world.mode(1), world.mode(2)),
            (Some(Mode::Dynamic), Some(Mode::Driven), None)
        );
        assert!(world.place(0, &[at(5.0)], 1.0).is_err(), "the solver moves a dynamic group");
        assert!(world.place(9, &[at(5.0)], 1.0).is_err(), "no such group");
        assert!(world.place(1, &[at_x(f32::NAN, 0.5)], 1.0).is_err());
        assert!(world.place(1, &[at_x(0.0, 0.5), at_x(f32::INFINITY, 0.5)], 1.0).is_err());
        assert_eq!(world.group_len(1), 1, "rows that fail add no body");
        // A limit that is not a number never flings a body: it is put there.
        world.place(1, &[at_x(3.0, 0.5)], f32::NAN).unwrap();
        world.step(DT, 1);
        run(&mut world, 0.5);
        assert!(world.placements(0)[0].translate[0].abs() < 1e-3);
        assert!((world.placements(1)[0].translate[0] - 3.0).abs() < 1e-5);
        // Obstacles: as many places as obstacles, all finite, or nothing moves.
        world.set_obstacles(vec![slab(placed_at(0.0, -5.0))]).unwrap();
        assert!(world.move_obstacles(&[], 1.0).is_err());
        assert!(world.move_obstacles(&[Place::default(), Place::default()], 1.0).is_err());
        assert!(world.move_obstacles(&[placed_at(f32::NAN, 0.0)], 1.0).is_err());
        let skew = Place { turn: [[f32::NAN; 3]; 3], ..Place::default() };
        assert!(world.move_obstacles(&[skew], 1.0).is_err());
        assert!(world.set_obstacles(vec![slab(skew)]).is_err());
        assert!(world.move_obstacles(&[], 1.0).is_ok(), "a failed set leaves no obstacle");
        // A turn that is no rotation (all zeros) still gives a place the solver can take.
        let flat = Place { turn: [[0.0; 3]; 3], ..Place::default() };
        if world.set_obstacles(vec![slab(flat)]).is_ok() {
            run(&mut world, 0.2);
        }
        assert!(world.placements(0)[0].translate.iter().all(|v| v.is_finite()));
    }
}
