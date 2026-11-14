//! Rigid bodies: a [`World`] of groups of bodies that fall, collide and come to rest, on a floor
//! and on triangle meshes. A group is copies of one [`Shape`] with one [`Material`]; bodies of
//! every group collide with each other. A group's [`Mode`] says who moves its bodies: the
//! solver, or the caller, whose bodies then push the solver's out of their way. Obstacles may
//! be moved too and carry along what they touch. Dynamic bodies can be pushed besides: by
//! [`Field`]s that fill the world (a wind, a pull to a point, a whirl), by a push of its own per
//! body ([`Push`]) and by a spring that pulls a body to a place ([`Spring`]). All of these count
//! as speed gained per second, whatever a body weighs. A [`Plane`] in the [`Settings`] keeps the
//! dynamic bodies in one plane, as on a wall. Space is penumbra's: metres, z up. Rotations
//! cross as degrees about x, then y, then z, the order `geo.transform` and `geo.instance` apply
//! them in.
//!
//! The solver is rapier3d. Everything that reaches it is checked here first. A number that is
//! not finite is an error or counts as nothing, as each function says. A finite number beyond
//! what the solver can work with is brought within the limits below: sizes, scales, places and
//! speeds from above as well as from below. Triangles must name vertices that exist, and counts
//! stay under the limits. Should a number in a body stop being one all the same, the solver
//! switches that body off, and [`World::disabled`] counts it.

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

/// The least a body collides as along an axis, from its middle, in metres: a box or a ball
/// that shape and scale make smaller is this large. The solver cannot hold a lighter body.
pub const MIN_HALF: f32 = 2.5e-5;

/// The largest scale of a body along an axis; larger counts as this.
pub const MAX_SCALE: f32 = 1e3;

/// The most a body collides as along an axis, from its middle, in metres, whatever shape and
/// scale say: a box, a ball or the way to its middle that they make larger is this large, and
/// a hull that reaches farther collides as a box of this size. A heavier body the solver
/// cannot weigh.
pub const MAX_SIZE: f32 = 1e6;

/// The farthest place from the origin along an axis, in metres. Farther counts as this.
pub const MAX_PLACE: f32 = 1e6;

/// The shortest time [`World::step`] takes as time, in seconds. Shorter counts as none.
pub const MIN_STEP: f32 = 1e-6;

/// The farthest a driven body or an obstacle turns within one step and still pushes, in
/// degrees. A turn beyond it is a jump: nothing says which way round such a turn went, and the
/// spin it stands for would throw whatever the body touches.
pub const MAX_TURN: f32 = 90.0;

/// How near to a body that jumped, left or changed its size a sleeping body is woken, in metres.
const NEAR: f32 = 0.1;

/// Most fields a world holds.
pub const MAX_FIELDS: usize = 64;

/// The most a field or a push speeds a body up, in metres per second squared, and the most a
/// push spins it up, in degrees per second squared. Larger counts as this.
pub const MAX_PUSH: f32 = 1e4;

/// The most a kick changes a body's speed by, in metres per second.
pub const MAX_KICK: f32 = 1e3;

/// The fastest a body is told to go, in metres per second: at its start, pushed, kicked or
/// pulled. The solver has a lower limit of its own for what a body then does: 400 metres per
/// second, and an eighth of a turn per solver step.
pub const MAX_SPEED: f32 = 1e4;

/// The stiffest spring, in metres per second squared per metre.
pub const MAX_STIFFNESS: f32 = 1e5;

/// Nearer than this to the middle of a point field or to the axis of a vortex the field fades
/// to nothing, in metres, so that a body at the middle is not thrown from side to side.
pub const FIELD_CORE: f32 = 0.2;

/// A body pushed by less than this may sleep, in metres per second squared.
const WAKE: f32 = 1e-3;

/// A body that nothing presses on and that moves slower than this may sleep, in metres per
/// second and in radians per second.
const DRIFT: f32 = 1e-4;

/// What a field does to the bodies it reaches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldKind {
    /// Pulls towards `position`; a negative strength pushes away from it.
    #[default]
    Point,
    /// Blows along `direction`.
    Wind,
    /// Drives round the axis through `position` along `direction`, counterclockwise seen from
    /// where the direction points to.
    Vortex,
}

/// How a field with a `radius` weakens away from its middle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Falloff {
    /// Full strength up to the radius.
    #[default]
    None,
    /// Full at the middle, evenly down to nothing at the radius.
    Linear,
}

/// A field: it speeds up every dynamic body it reaches, a heavy one as much as a light one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub kind: FieldKind,
    /// Metres per second squared for a field that is set ([`World::set_fields`]), metres per
    /// second for a kick ([`World::kick`]). Negative turns the field round.
    pub strength: f32,
    /// The middle of a point field, a point on the axis of a vortex, the middle of what a wind
    /// with a radius reaches.
    pub position: [f32; 3],
    /// Where a wind blows, the axis of a vortex. Only the direction counts. A wind without one
    /// (all zeros) blows nowhere, a vortex without one drives nothing.
    pub direction: [f32; 3],
    /// How far the field reaches, in metres: from `position`, for a vortex from its axis. Zero
    /// or less: everywhere.
    pub radius: f32,
    pub falloff: Falloff,
}

impl Default for Field {
    fn default() -> Self {
        Self {
            kind: FieldKind::Point,
            strength: 0.0,
            position: [0.0; 3],
            direction: [0.0, 0.0, 1.0],
            radius: 0.0,
            falloff: Falloff::None,
        }
    }
}

impl Field {
    /// The field with its numbers inside what the solver takes and its direction of length 1
    /// (zero for a direction without length: such a wind blows nowhere). None when a number
    /// is not finite.
    fn checked(self, most: f32) -> Option<Self> {
        let mut numbers = [self.strength, self.radius].into_iter().chain(self.position).chain(self.direction);
        if !numbers.all(f32::is_finite) {
            return None;
        }
        let direction = vector(self.direction).normalize_or_zero();
        Some(Self {
            strength: self.strength.clamp(-most, most),
            position: self.position.map(|v| v.clamp(-MAX_PLACE, MAX_PLACE)),
            direction: [direction.x, direction.y, direction.z],
            radius: self.radius.max(0.0),
            ..self
        })
    }

    /// What a checked field does to a body at `at`: a speed gained per second, or for a kick
    /// a change of speed.
    fn at(&self, at: Vector) -> Vector {
        let from = at - vector(self.position);
        let axis = vector(self.direction);
        // The way from the middle, or from the axis, that the radius is measured along.
        let out = if self.kind == FieldKind::Vortex { from - axis * from.dot(axis) } else { from };
        let far = out.length();
        let reach = match self.falloff {
            _ if self.radius <= 0.0 => 1.0,
            _ if far >= self.radius => 0.0,
            Falloff::None => 1.0,
            Falloff::Linear => 1.0 - far / self.radius,
        };
        let core = (far / FIELD_CORE).min(1.0);
        let pull = match self.kind {
            FieldKind::Wind => axis * reach,
            FieldKind::Point if far > 0.0 => out * (-core * reach / far),
            FieldKind::Vortex if far > 0.0 => axis.cross(out) * (core * reach / far),
            _ => Vector::ZERO,
        };
        pull * self.strength
    }
}

/// What pushes one body: a speed gained per second along x, y and z, and a spin gained per
/// second about them, whatever the body weighs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Push {
    /// Metres per second squared.
    pub push: [f32; 3],
    /// Degrees per second squared about x, y and z.
    pub twist: [f32; 3],
}

/// The spring that pulls the bodies of a group to their targets ([`World::set_targets`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// How much of the spring acts, 0 to 1. A pulled body feels that much less of gravity, so
    /// at 1 it comes to rest at its target and not under it. 0: the bodies are let go.
    pub pull: f32,
    /// The speed a body gains per second for every metre it is from its target, at `pull` 1.
    pub stiffness: f32,
    /// How much the pull brakes: 0 lets a body swing about its target for ever, 1 brings it
    /// there without swinging past, more creeps.
    pub damping: f32,
}

impl Default for Spring {
    fn default() -> Self {
        Self { pull: 1.0, stiffness: 40.0, damping: 1.0 }
    }
}

impl Spring {
    /// The spring with its numbers inside what the solver takes; one that is not a number
    /// lets go.
    fn checked(self) -> Self {
        let n = |v: f32, hi: f32| if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
        Self {
            pull: n(self.pull, 1.0),
            stiffness: n(self.stiffness, MAX_STIFFNESS),
            damping: n(self.damping, 100.0),
        }
    }
}

/// A plane through the world that dynamic bodies stay in: each keeps its place across the
/// plane and turns about the plane's normal only. Bodies at different places across the plane
/// move in parallel layers. Driven and fixed bodies, obstacles and the floor are not held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Plane {
    /// No plane: bodies move and turn every way.
    #[default]
    Off,
    /// Bodies keep their z and turn about z only.
    Xy,
    /// Bodies keep their y and turn about y only.
    Xz,
    /// Bodies keep their x and turn about x only.
    Yz,
}

impl Plane {
    /// The plane's normal: the axis a body keeps its place along. None without a plane.
    fn normal(self) -> Option<Vector> {
        match self {
            Plane::Off => None,
            Plane::Xy => Some(Vector::Z),
            Plane::Xz => Some(Vector::Y),
            Plane::Yz => Some(Vector::X),
        }
    }

    /// What the solver holds still for a body in the plane: moving along the normal and the
    /// two turns that would tilt the body out of the plane.
    fn locks(self) -> LockedAxes {
        match self {
            Plane::Off => LockedAxes::empty(),
            Plane::Xy => {
                LockedAxes::TRANSLATION_LOCKED_Z
                    | LockedAxes::ROTATION_LOCKED_X
                    | LockedAxes::ROTATION_LOCKED_Y
            }
            Plane::Xz => {
                LockedAxes::TRANSLATION_LOCKED_Y
                    | LockedAxes::ROTATION_LOCKED_X
                    | LockedAxes::ROTATION_LOCKED_Z
            }
            Plane::Yz => {
                LockedAxes::TRANSLATION_LOCKED_X
                    | LockedAxes::ROTATION_LOCKED_Y
                    | LockedAxes::ROTATION_LOCKED_Z
            }
        }
    }

    /// `speed` without its part along the normal. The solver's locks stop forces along the
    /// normal, but a speed set on a body moves it all the same.
    fn along(self, speed: Vector) -> Vector {
        self.normal().map_or(speed, |n| speed - n * speed.dot(n))
    }

    /// Only the part of `spin` about the normal: the turn that keeps a body in the plane.
    fn about(self, spin: Vector) -> Vector {
        self.normal().map_or(spin, |n| n * spin.dot(n))
    }
}

/// What may change while the bodies move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Metres per second squared.
    pub gravity: [f32; 3],
    /// The plane dynamic bodies stay in, or none.
    pub plane: Plane,
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
            plane: Plane::Off,
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
            plane: self.plane,
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
    /// The size the body has now: as it started, or for a driven or fixed body as its row said
    /// last. Negative along an axis for a body mirrored there.
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
    /// What pushes body k, by k; a body beyond the last has none.
    pushes: Vec<Push>,
    /// Where body k is pulled to, by k; a body beyond the last, or with none, is not pulled.
    targets: Vec<Option<Vector>>,
    spring: Spring,
    /// What a body of a hull group collides as, per scale, since a body's scale is part of
    /// its collision shape.
    hulls: Vec<([f32; 3], Made)>,
}

/// What the bodies of a hull group collide as at one scale.
#[derive(Clone)]
enum Made {
    Hull(SharedShape),
    /// The box around the points, where they enclose too little room at this scale for a
    /// hull: its middle and its half sizes. Kept, so that the hull is not tried again.
    Box(Vector, Vector),
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
    /// Checked ([`Field::checked`]).
    fields: Vec<Field>,
    /// Whether the last step pushed a body, so that the next one looks at them once more even
    /// when nothing pushes any longer: a body that slept under a push must wake.
    pushed: bool,
}

impl World {
    /// A world without bodies.
    pub fn new(settings: Settings) -> Self {
        let settings = settings.checked();
        let mut world = PhysicsWorld::new();
        world.gravity = vector(settings.gravity);
        let mut out = Self {
            world,
            settings,
            groups: Vec::new(),
            count: 0,
            floor: None,
            obstacles: Vec::new(),
            fields: Vec::new(),
            pushed: false,
        };
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
                pushes: Vec::new(),
                targets: Vec::new(),
                spring: Spring::default().checked(),
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
    /// scale from `start` and nothing else. A place beyond [`MAX_PLACE`], a scale outside
    /// [`MIN_SCALE`] to [`MAX_SCALE`] and a speed beyond [`MAX_SPEED`] count as those; a
    /// negative scale mirrors the body along that axis.
    ///
    /// A hull that encloses too little room at this scale collides as the box around it.
    ///
    /// An error, and the world stays as it was: for a number that is not finite, for a group
    /// that does not exist and for a world that is full.
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
        let collider = g.collider(scale);
        let plane = self.settings.plane;
        let body = match g.mode {
            Mode::Dynamic => RigidBodyBuilder::dynamic()
                .linvel(plane.along(capped(start.velocity, MAX_SPEED)))
                .angvel(plane.about(capped(start.spin.map(f32::to_radians), MAX_SPEED)))
                .locked_axes(plane.locks())
                .gyroscopic_forces_enabled(plane == Plane::Off)
                .linear_damping(self.settings.damping)
                .angular_damping(self.settings.spin_damping),
            Mode::Driven => RigidBodyBuilder::kinematic_position_based(),
            Mode::Fixed => RigidBodyBuilder::fixed(),
        };
        let (body, collider) = self.world.insert(body.pose(pose(start)), collider);
        if g.mode != Mode::Dynamic {
            // The solver does not wake a sleeping body that this one appears inside of, and
            // once the step is over it no longer sees that the two overlap.
            wake_near(&mut self.world, body, None);
        }
        g.bodies.push(Body { body, collider, scale, target: None });
        self.count += 1;
        Ok(())
    }

    /// Says where the bodies of a driven or fixed `group` are to be: body k goes to row k, with
    /// the row's place, turn and scale. Rows beyond the bodies add bodies, bodies beyond the
    /// rows leave the world.
    ///
    /// A driven body travels to its row within the next [`World::step`] and pushes what is in
    /// its way with the speed that takes. A row farther than `jump` metres from its body, or
    /// turned by more than [`MAX_TURN`] against it, puts the body there at once instead, and
    /// nothing is pushed; so does every row while `jump` is 0. A fixed body is always put
    /// there at once. A body takes a new scale at once: what it then overlaps is pushed out
    /// of it by the solver.
    ///
    /// An error for a group that does not exist, for a dynamic group and for rows that are
    /// not finite: the world stays as it was. An error also for rows the world has no room
    /// for: the rows before them are placed.
    pub fn place(&mut self, group: usize, rows: &[Start], jump: f32) -> Result<(), String> {
        self.place_held(group, rows, &[], jump)
    }

    /// As [`World::place`], with rows to pass over: where `held` says true for row k, body k
    /// stays as it is, and keeps its number. A body that row k adds takes the row all the same.
    pub fn place_held(
        &mut self,
        group: usize,
        rows: &[Start],
        held: &[bool],
        jump: f32,
    ) -> Result<(), String> {
        if !rows.iter().all(finite) {
            return Err("a body's numbers must be finite".into());
        }
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else {
            return Err(format!("there is no group {group}"));
        };
        if g.mode == Mode::Dynamic {
            return Err(format!("the solver moves the bodies of group {group}"));
        }
        for gone in g.bodies.drain(rows.len().min(g.bodies.len())..) {
            // What rested on a body that leaves must fall on.
            wake_near(&mut self.world, gone.body, None);
            self.world.remove_body(gone.body);
            self.count = self.count.saturating_sub(1);
        }
        let mode = g.mode;
        for k in 0..g.bodies.len().min(rows.len()) {
            if held.get(k).copied().unwrap_or(false) {
                continue;
            }
            let (Some(row), Some(scale)) = (rows.get(k), g.bodies.get(k).map(|b| b.scale)) else { break };
            let wanted = sized(row.scale);
            let resized = (wanted != scale).then(|| g.collider(wanted));
            let Some(body) = g.bodies.get_mut(k) else { break };
            if let Some(collider) = resized {
                // What rested on a body that shrinks must fall on, and what sleeps where a
                // body grows to must make room.
                wake_near(&mut self.world, body.body, None);
                self.world.remove_collider(body.collider);
                body.collider = self.world.insert_collider(collider, Some(body.body));
                body.scale = wanted;
                wake_near(&mut self.world, body.body, None);
            }
            let to = pose(row);
            match mode {
                Mode::Driven => aim(&mut self.world, body.body, &mut body.target, to, jump),
                _ => put(&mut self.world, body.body, to),
            };
        }
        let have = g.bodies.len();
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
            if o.vertices.iter().flatten().any(|v| v.abs() > MAX_PLACE) {
                let why = format!("an obstacle reaches farther than {MAX_PLACE} m from its origin");
                return failed(self, why);
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
    /// One whose place is farther than `jump` metres from where it stands, or turned by more
    /// than [`MAX_TURN`], is put there at once and pushes nothing; so is every obstacle that
    /// moves while `jump` is 0. An error, and no obstacle moves, when the count differs or a
    /// place is not finite.
    pub fn move_obstacles(&mut self, places: &[Place], jump: f32) -> Result<(), String> {
        if places.len() != self.obstacles.len() {
            return Err(format!("{} places for {} obstacles", places.len(), self.obstacles.len()));
        }
        let Some(poses) = places.iter().map(pose_of).collect::<Option<Vec<Pose>>>() else {
            return Err("an obstacle is to stand at a place that is not a number".into());
        };
        for (fixture, to) in self.obstacles.iter_mut().zip(poses) {
            if let Some(f) = fixture {
                aim(&mut self.world, f.body, &mut f.target, to, jump);
            }
        }
        Ok(())
    }

    /// Replaces every field. The fields act on dynamic bodies in every [`World::step`]
    /// until they are replaced: `strength` is the speed a body gains per second. An error, and
    /// the fields stay as they were, for more than [`MAX_FIELDS`] and for a number that is not
    /// finite.
    pub fn set_fields(&mut self, fields: &[Field]) -> Result<(), String> {
        if fields.len() > MAX_FIELDS {
            return Err(format!("a world holds {MAX_FIELDS} fields at most"));
        }
        let Some(checked) = fields.iter().map(|f| f.checked(MAX_PUSH)).collect::<Option<Vec<Field>>>() else {
            return Err("a field's numbers must be finite".into());
        };
        // A field switched off costs nothing, and lets the bodies sleep.
        self.fields = checked.into_iter().filter(|f| f.strength != 0.0).collect();
        Ok(())
    }

    /// Changes the speed of every dynamic body `field` reaches, once and at once: `strength`
    /// is that change in metres per second, at most [`MAX_KICK`]. Bodies at rest are woken.
    /// An error, and no body is touched, for a number that is not finite.
    pub fn kick(&mut self, field: &Field) -> Result<(), String> {
        let Some(field) = field.checked(MAX_KICK) else {
            return Err("a field's numbers must be finite".into());
        };
        let dynamic = self.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic);
        for b in dynamic.flat_map(|g| &g.bodies) {
            let Some(body) = self.world.bodies.get_mut(b.body) else { continue };
            let change = self.settings.plane.along(field.at(body.translation()));
            if change != Vector::ZERO {
                body.set_linvel((body.linvel() + change).clamp_length_max(MAX_SPEED), true);
            }
        }
        Ok(())
    }

    /// Says what pushes the bodies of `group` from now on: row k pushes body k. A body without
    /// a row is not pushed, rows beyond the bodies wait for bodies to come. A number that is
    /// not finite counts as 0, one beyond [`MAX_PUSH`] as that. Only a dynamic group is
    /// pushed. Nothing happens for a group that does not exist.
    pub fn set_pushes(&mut self, group: usize, pushes: &[Push]) {
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else { return };
        let n = |v: f32| if v.is_finite() { v.clamp(-MAX_PUSH, MAX_PUSH) } else { 0.0 };
        g.pushes.clear();
        let rows = pushes.iter().take(MAX_BODIES);
        g.pushes.extend(rows.map(|p| Push { push: p.push.map(n), twist: p.twist.map(n) }));
    }

    /// Says where the bodies of `group` are pulled to from now on, and how: body k to row k,
    /// by `spring`. A body without a row, or with a row that is none or not finite, is not
    /// pulled. Only a dynamic group is pulled. Nothing happens for a group that does not exist.
    pub fn set_targets(&mut self, group: usize, targets: &[Option<[f32; 3]>], spring: Spring) {
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else { return };
        g.spring = spring.checked();
        g.targets.clear();
        let finite = |t: &[f32; 3]| t.iter().all(|v| v.is_finite());
        let within = |t: [f32; 3]| vector(t.map(|v| v.clamp(-MAX_PLACE, MAX_PLACE)));
        g.targets.extend(targets.iter().take(MAX_BODIES).map(|t| t.filter(finite).map(within)));
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
        if old.plane != settings.plane {
            // Each body stays where it is across the new plane, and loses the speed and the
            // spin that would take it out.
            let plane = settings.plane;
            let dynamic = self.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic);
            for b in dynamic.flat_map(|g| &g.bodies) {
                let Some(body) = self.world.bodies.get_mut(b.body) else { continue };
                body.set_locked_axes(plane.locks(), false);
                // The solver's turning of a spinning body about its own axes ignores the
                // locks, and in a plane it has nothing to do.
                body.enable_gyroscopic_forces(plane == Plane::Off);
                let (speed, spin) = (body.linvel(), body.angvel());
                body.set_linvel(plane.along(speed), false);
                body.set_angvel(plane.about(spin), false);
            }
        }
        // Bodies at rest sleep; a new gravity or a floor that left must reach them too.
        self.world.wake_up_all(true);
    }

    /// Moves time on by `seconds`, in `substeps` equal steps (more keeps fast or stacked
    /// bodies steadier). Driven bodies and moved obstacles travel to their places evenly over
    /// the whole time, so the speed they push with does not depend on `substeps`.
    ///
    /// For a time that is not finite or shorter than [`MIN_STEP`] no body moves on its own.
    /// Driven bodies and moved obstacles are then put at their places at once and push
    /// nothing: without time there is no speed to push with.
    pub fn step(&mut self, seconds: f32, substeps: u32) {
        let mut moves: Vec<(RigidBodyHandle, Pose, Pose)> = Vec::new();
        let bodies = self.groups.iter_mut().flatten().flat_map(|g| &mut g.bodies);
        let driven = bodies.map(|b| (b.body, &mut b.target));
        let moved = self.obstacles.iter_mut().flatten().map(|f| (f.body, &mut f.target));
        for (handle, target) in driven.chain(moved) {
            let Some(to) = target.take() else { continue };
            if let Some(from) = self.world.bodies.get(handle).map(|b| *b.position()) {
                moves.push((handle, from, short(&from, to)));
            }
        }
        // The solver divides by the time of a step: one too short to divide by gives every
        // body it moves a speed that is no number.
        if !seconds.is_finite() || seconds < MIN_STEP {
            for (handle, _, to) in moves {
                put(&mut self.world, handle, to);
            }
            return;
        }
        let n = substeps.clamp(1, MAX_SUBSTEPS);
        self.world.integration_parameters.dt = seconds / n as f32;
        for k in 1..=n {
            self.push(seconds / n as f32);
            self.keep_drifting();
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
        for (handle, _, _) in &moves {
            wake_near(&mut self.world, *handle, None);
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

    /// How many bodies and obstacles the solver has switched off because a number in them
    /// stopped being one. Such a body stays where it was, collides with nothing and still
    /// counts as a body. Only a world built anew brings it back.
    pub fn disabled(&self) -> usize {
        let on = |body: RigidBodyHandle, collider: ColliderHandle| {
            self.world.bodies.get(body).is_some_and(RigidBody::is_enabled)
                && self.world.colliders.get(collider).is_some_and(Collider::is_enabled)
        };
        let bodies = self.groups.iter().flatten().flat_map(|g| &g.bodies);
        let off = bodies.filter(|b| !on(b.body, b.collider)).count();
        off + self.obstacles.iter().flatten().filter(|f| !on(f.body, f.collider)).count()
    }

    fn group(&self, group: usize) -> Option<&Group> {
        self.groups.get(group).and_then(Option::as_ref)
    }

    /// Lets the fields, the pushes and the springs act on the dynamic bodies for one solver
    /// step of `dt` seconds, which is to follow: they change the bodies' speeds.
    ///
    /// A body that something pushes is kept awake. The solver lets a slow body sleep whether
    /// or not it has come to rest, so a body under a weak push would stop on its way. A heap
    /// in a wind therefore costs its full time on every step.
    fn push(&mut self, dt: f32) {
        let pushing = |g: &Group| {
            let pulled = !g.targets.is_empty() && g.spring.pull > 0.0;
            g.mode == Mode::Dynamic && (!g.pushes.is_empty() || pulled)
        };
        let any = !self.fields.is_empty() || self.groups.iter().flatten().any(pushing);
        if !any && !self.pushed {
            return;
        }
        self.pushed = any;
        let plane = self.settings.plane;
        for g in self.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic) {
            let spring = g.spring;
            // The spring as a body feels it: how hard it pulls per metre, how hard it brakes
            // per metre a second. The braking keeps its share of the pull at any `pull`.
            let stiffness = spring.pull * spring.stiffness;
            let brake = 2.0 * spring.damping * stiffness.sqrt();
            for (k, b) in g.bodies.iter().enumerate() {
                let Some(body) = self.world.bodies.get_mut(b.body) else { continue };
                let at = body.translation();
                let own = g.pushes.get(k).copied().unwrap_or_default();
                let target = g.targets.get(k).copied().flatten().filter(|_| stiffness > 0.0);
                let lift = if target.is_some() { spring.pull } else { 0.0 };
                let mut push = vector(own.push);
                for field in &self.fields {
                    push += field.at(at);
                }
                let push = plane.along(push.clamp_length_max(MAX_PUSH));
                let twist = plane.about(vector(own.twist.map(f32::to_radians)));
                // A pulled body feels less of gravity: at its target nothing pushes it, and
                // it may sleep there, in the air.
                if body.gravity_scale() != 1.0 - lift {
                    body.set_gravity_scale(1.0 - lift, true);
                    // Awake for good: a body merely stirred sleeps again within the step.
                    body.wake_up(true);
                }
                let pull = plane.along(target.map_or(Vector::ZERO, |t| (t - at) * stiffness));
                if (push + pull).length() > WAKE || twist.length() > WAKE {
                    body.wake_up(true);
                } else if body.is_sleeping() {
                    continue;
                }
                let mut speed = body.linvel();
                let mut spin = body.angvel();
                if let Some(target) = target {
                    // The spring's pull and its braking, taken at the end of the step: this
                    // holds for any stiffness, where the pull taken at its start would throw
                    // the body farther with every step once the spring is stiff.
                    let ease = 1.0 + dt * brake + dt * dt * stiffness;
                    speed = (speed + (target - at) * (dt * stiffness)) / ease;
                    // A pulled body loses its spin as it loses its swing.
                    spin /= 1.0 + dt * brake;
                }
                speed += push * dt;
                spin += twist * dt;
                // A target beside the plane pulls across it too.
                let speed = plane.along(speed.clamp_length_max(MAX_SPEED));
                let spin = plane.about(spin.clamp_length_max(MAX_SPEED));
                if speed.is_finite() && spin.is_finite() {
                    body.set_linvel(speed, false);
                    body.set_angvel(spin, false);
                }
            }
        }
    }

    /// Keeps awake every dynamic body that moves and that nothing holds. The solver lets a slow
    /// body sleep whether or not it has come to rest, and a sleeping body loses its speed: a
    /// ball that drifts slowly through empty space, or slowly past a wall, would stop dead. A
    /// body that something presses on may still sleep, so a heap comes to rest as before.
    fn keep_drifting(&mut self) {
        let dynamic = self.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic);
        for b in dynamic.flat_map(|g| &g.bodies) {
            let Some(body) = self.world.bodies.get(b.body) else { continue };
            let moving = body.linvel().length() > DRIFT || body.angvel().length() > DRIFT;
            if body.is_sleeping() || !moving {
                continue;
            }
            // A contact counts once it pushed in the last step: the solver also keeps contacts
            // with what is merely near.
            if self.world.contact_pairs_with(b.collider).any(|pair| pair.total_impulse_magnitude() > 0.0) {
                continue;
            }
            if let Some(body) = self.world.bodies.get_mut(b.body) {
                body.wake_up(true);
            }
        }
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
    fn collider(&mut self, scale: [f32; 3]) -> ColliderBuilder {
        self.shape_at(scale)
            .friction(self.material.friction)
            .restitution(self.material.bounce)
            .restitution_combine_rule(CoefficientCombineRule::Max)
            .density(self.material.density)
    }

    /// The collision shape at `scale`. A hull that encloses too little room at this scale
    /// gives way to the box around its points, so that every scale has a shape.
    fn shape_at(&mut self, scale: [f32; 3]) -> ColliderBuilder {
        // A negative scale mirrors: it turns the middle and the points over, and a size stays
        // a size.
        let [sx, sy, sz] = scale.map(f32::abs);
        match &self.shape {
            Shape::Box { center, half } => {
                let [hx, hy, hz] = *half;
                let [x, y, z] = [hx * sx, hy * sy, hz * sz].map(|h| h.clamp(MIN_HALF, MAX_SIZE));
                ColliderBuilder::cuboid(x, y, z).translation(offset(*center, scale))
            }
            Shape::Sphere { center, radius } => {
                // A ball stays a ball: the widest scale decides.
                let radius = (radius * sx.max(sy).max(sz)).clamp(MIN_HALF, MAX_SIZE);
                ColliderBuilder::ball(radius).translation(offset(*center, scale))
            }
            Shape::Hull { points } => {
                let made = match self.hulls.iter().find(|(s, _)| *s == scale) {
                    Some((_, made)) => made.clone(),
                    None => {
                        let scaled: Vec<Vector> = points.iter().map(|p| scaled(*p, scale)).collect();
                        let made = hull(&scaled).map_or_else(|_| boxed(&scaled), Made::Hull);
                        if self.hulls.len() >= MAX_HULLS {
                            self.hulls.clear();
                        }
                        self.hulls.push((scale, made.clone()));
                        made
                    }
                };
                match made {
                    Made::Hull(shape) => ColliderBuilder::new(shape),
                    Made::Box(center, half) => {
                        ColliderBuilder::cuboid(half.x, half.y, half.z).translation(center)
                    }
                }
            }
        }
    }
}

/// Whether every number of `start` is finite.
fn finite(start: &Start) -> bool {
    let all = [start.translate, start.rotate_degrees, start.scale, start.velocity, start.spin];
    all.iter().flatten().all(|v| v.is_finite())
}

/// A scale as a body takes it: no nearer to zero than [`MIN_SCALE`], no farther than
/// [`MAX_SCALE`]. Its sign stays: a negative scale mirrors.
fn sized(scale: [f32; 3]) -> [f32; 3] {
    scale.map(|s| {
        let size = s.abs().clamp(MIN_SCALE, MAX_SCALE);
        if s < 0.0 { -size } else { size }
    })
}

/// `p` as a place the solver takes: no farther out than [`MAX_PLACE`].
fn within(p: [f32; 3]) -> Vector {
    vector(p.map(|v| v.clamp(-MAX_PLACE, MAX_PLACE)))
}

/// `v` no longer than `most`, however large its numbers.
fn capped(v: [f32; 3], most: f32) -> Vector {
    vector(v.map(|x| x.clamp(-most, most))).clamp_length_max(most)
}

/// Where `start` places a body.
fn pose(start: &Start) -> Pose {
    Pose::from_parts(within(start.translate), rotation(start.rotate_degrees))
}

/// `place` as the solver takes it; none when a number of it is not finite.
fn pose_of(place: &Place) -> Option<Pose> {
    let [x, y, z] = place.turn;
    let turn = Rotation::from_mat3(&Matrix::from_cols(vector(x), vector(y), vector(z)).transpose());
    let finite = place.translate.iter().all(|v| v.is_finite());
    let pose = Pose::from_parts(within(place.translate), turn.normalize());
    (finite && pose.is_finite()).then_some(pose)
}

/// `to` written so that the turn from `from` to it is the short way round. A rotation has two
/// quaternions, one the negative of the other. The solver takes the speed of a body it is
/// told to move from the two as they are written, and with the far one that speed is nearly a
/// full turn the other way round within one step.
fn short(from: &Pose, to: Pose) -> Pose {
    if from.rotation.dot(to.rotation) < 0.0 { Pose::from_parts(to.translation, -to.rotation) } else { to }
}

/// Wakes the dynamic bodies that sleep where the body `handle` is, or within [`NEAR`] of it:
/// those that rest on it or that it stands inside of. With `at`, where the body would be if
/// it stood there.
fn wake_near(world: &mut PhysicsWorld, handle: RigidBodyHandle, at: Option<Pose>) {
    let colliders = world.bodies.get(handle).map(RigidBody::colliders).unwrap_or_default();
    let mut reached: Vec<RigidBodyHandle> = Vec::new();
    for collider in colliders.iter().filter_map(|c| world.colliders.get(*c)) {
        let aabb = match at {
            Some(pose) => {
                let own = collider.position_wrt_parent().copied().unwrap_or(Pose::IDENTITY);
                collider.shape().compute_aabb(&(pose * own))
            }
            None => collider.compute_aabb(),
        };
        let wide = Aabb::new(aabb.mins - Vector::splat(NEAR), aabb.maxs + Vector::splat(NEAR));
        let near = world.intersect_aabb_conservative(wide, QueryFilter::only_dynamic());
        reached.extend(near.filter_map(|(_, collider)| collider.parent()));
    }
    for handle in reached {
        world.wake_up(handle, true);
    }
}

/// Puts the body `handle` at `to` at once, without a speed. Returns whether it stood elsewhere.
/// What slept on it or beside it is then woken, and falls on.
fn put(world: &mut PhysicsWorld, handle: RigidBodyHandle, to: Pose) -> bool {
    let Some(body) = world.bodies.get(handle) else { return false };
    let to = short(body.position(), to);
    if *body.position() == to && *body.next_position() == to {
        return false;
    }
    wake_near(world, handle, None);
    if let Some(body) = world.bodies.get_mut(handle) {
        body.set_position(to, true);
    }
    // What sleeps where the body lands must make room. The solver would not wake it: it
    // wakes a sleeping body only through a touch it already knows of.
    wake_near(world, handle, Some(to));
    true
}

/// Sends the body `handle`, which the caller moves, to `to`: it is there after the next step,
/// by way of `target`. Farther than `jump` metres from where it stands, or turned by more than
/// [`MAX_TURN`], it is put there at once instead; so it is whenever `jump` is 0. Returns
/// whether it was put.
fn aim(
    world: &mut PhysicsWorld,
    handle: RigidBodyHandle,
    target: &mut Option<Pose>,
    to: Pose,
    jump: f32,
) -> bool {
    let Some(from) = world.bodies.get(handle).map(|b| *b.position()) else { return false };
    let to = short(&from, to);
    // Not a number counts as no way at all, so a bad `jump` never flings a body.
    let near = jump > 0.0 && (to.translation - from.translation).length() <= jump;
    // The cosine of half the angle between the two turns.
    let slow = from.rotation.dot(to.rotation) >= (0.5 * MAX_TURN.to_radians()).cos();
    if near && slow {
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

/// Where the middle of a box or a ball lies in a body of `scale`, no farther out than
/// [`MAX_SIZE`].
fn offset(center: [f32; 3], scale: [f32; 3]) -> Vector {
    scaled(center, scale).clamp(Vector::splat(-MAX_SIZE), Vector::splat(MAX_SIZE))
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

/// The corners of the box around `points`: the least and the most along each axis.
fn bounds(points: &[Vector]) -> (Vector, Vector) {
    points.iter().fold((Vector::MAX, Vector::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
}

/// The box around `points`, no thinner and no larger along an axis than a body may be.
fn boxed(points: &[Vector]) -> Made {
    let (lo, hi) = bounds(points);
    let most = Vector::splat(MAX_SIZE);
    // Halved before they are taken from each other: the difference of two far corners may be
    // more than a number holds.
    let half = (hi * 0.5 - lo * 0.5).clamp(Vector::splat(MIN_HALF), most);
    Made::Box((lo * 0.5 + hi * 0.5).clamp(-most, most), half)
}

/// The convex hull of `points`. An error for points that enclose no room (all in one plane, on
/// one line or in one place): such a hull weighs nothing, and a body without weight hangs in
/// the air. An error also for a hull so small that the lightest body of it weighs less than
/// the solver can divide by, and for one that reaches farther than [`MAX_SIZE`]: the solver
/// would switch such a body off.
fn hull(points: &[Vector]) -> Result<SharedShape, String> {
    const FLAT: &str = "the shape is flat: no convex hull encloses it";
    // What the solver still divides by, with room to spare; the smallest box weighs more.
    const LEAST: f32 = 1e-27;
    let (lo, hi) = bounds(points);
    if hi.max(-lo).max_element() > MAX_SIZE {
        return Err(format!("the shape reaches farther than {MAX_SIZE} m from its origin"));
    }
    let shape = SharedShape::convex_hull(points).ok_or(FLAT)?;
    let reach = (hi - lo).length();
    let room = shape.mass_properties(1.0).mass();
    if !(room.is_finite() && reach.is_finite() && room > 1e-6 * reach * reach * reach) {
        return Err(FLAT.into());
    }
    let [light, heavy] = DENSITY_RANGE.map(|density| shape.mass_properties(density));
    let weighs = |m: &MassProperties| m.mass().min(m.principal_inertia().min_element());
    if weighs(&light) > LEAST && weighs(&heavy).is_finite() {
        Ok(shape)
    } else {
        Err("the shape is too small for a convex hull".into())
    }
}

/// `shape` as a world takes it: sizes away from zero and no larger than [`MAX_SIZE`], a hull
/// thinned to [`MAX_HULL_POINTS`]. An error for numbers that are not finite, for a hull that
/// encloses no room or too little, and for one that reaches farther than [`MAX_SIZE`].
pub fn check(shape: Shape) -> Result<Shape, String> {
    let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
    match shape {
        Shape::Box { center, half } => {
            if !finite(&center) || !finite(&half) {
                return Err("the box's numbers must be finite".into());
            }
            let center = center.map(|c| c.clamp(-MAX_SIZE, MAX_SIZE));
            Ok(Shape::Box { center, half: half.map(|h| h.abs().clamp(MIN_SCALE * 0.5, MAX_SIZE)) })
        }
        Shape::Sphere { center, radius } => {
            if !finite(&center) || !radius.is_finite() {
                return Err("the sphere's numbers must be finite".into());
            }
            let center = center.map(|c| c.clamp(-MAX_SIZE, MAX_SIZE));
            Ok(Shape::Sphere { center, radius: radius.abs().clamp(MIN_SCALE * 0.5, MAX_SIZE) })
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
        assert!(sound(&world));
        // Under gravity the body falls and lands all the same.
        world.apply(Settings::default());
        run(&mut world, 2.0);
        assert!((world.placements(0)[0].translate[2] - 0.5).abs() < 0.02 && sound(&world));
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
        assert!(sound(&world));
    }

    /// A world without gravity and without a floor, its group 0 dynamic balls of 1 m.
    fn space() -> World {
        world_of(ball(), Settings { gravity: [0.0; 3], floor: None, ..Settings::default() })
    }

    fn wind(strength: f32) -> Field {
        Field { kind: FieldKind::Wind, strength, direction: [2.0, 0.0, 0.0], ..Field::default() }
    }

    fn point(strength: f32) -> Field {
        Field { kind: FieldKind::Point, strength, ..Field::default() }
    }

    #[test]
    fn a_wind_moves_dynamic_bodies_alike_and_leaves_driven_and_fixed_ones_alone() {
        let mut world = space();
        let heavy = Material { density: 50.0, ..Material::default() };
        world.set_group(1, unit_box(), heavy, Mode::Dynamic).unwrap();
        world.set_group(2, unit_box(), Material::default(), Mode::Driven).unwrap();
        world.set_group(3, unit_box(), Material::default(), Mode::Fixed).unwrap();
        world.add(0, &at(0.0)).unwrap();
        world.add(1, &Start { translate: [0.0, 5.0, 0.0], scale: [3.0; 3], ..Start::default() }).unwrap();
        world.add(2, &at(10.0)).unwrap();
        world.add(3, &at(20.0)).unwrap();
        // 2 m/s^2 for one second: 1 m, whatever the body weighs and however long the
        // direction is.
        world.set_fields(&[wind(2.0)]).unwrap();
        run(&mut world, 1.0);
        let (light, heavy) = (world.placements(0)[0].translate, world.placements(1)[0].translate);
        assert!((light[0] - 1.0).abs() < 0.05 && light[1].abs() < 1e-4, "{light:?}");
        assert!((heavy[0] - light[0]).abs() < 1e-3, "a big heavy box went as far: {heavy:?}");
        assert_eq!(world.placements(2)[0].translate, [0.0, 0.0, 10.0]);
        assert_eq!(world.placements(3)[0].translate, [0.0, 0.0, 20.0]);
        // The same way in four solver steps a frame.
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.set_fields(&[wind(2.0)]).unwrap();
        for _ in 0..60 {
            world.step(DT, 4);
        }
        assert!((world.placements(0)[0].translate[0] - 1.0).abs() < 0.05);
        // Without fields the body keeps its speed and gains none; without time nothing acts.
        world.set_fields(&[]).unwrap();
        let before = world.placements(0)[0].translate[0];
        run(&mut world, 1.0);
        assert!((world.placements(0)[0].translate[0] - before - 2.0).abs() < 0.05);
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.set_fields(&[wind(50.0)]).unwrap();
        world.step(0.0, 1);
        world.step(f32::NAN, 1);
        world.set_fields(&[]).unwrap();
        run(&mut world, 1.0);
        assert_eq!(world.placements(0)[0].translate, [0.0; 3]);
    }

    #[test]
    fn a_wind_with_a_radius_reaches_only_so_far() {
        let mut world = space();
        for x in [0.0, 1.5, 4.0] {
            world.add(0, &at_x(x, 0.0)).unwrap();
        }
        let field = |falloff| Field {
            kind: FieldKind::Wind,
            strength: 2.0,
            direction: [0.0, 0.0, 1.0],
            radius: 3.0,
            falloff,
            ..Field::default()
        };
        world.set_fields(&[field(Falloff::None)]).unwrap();
        run(&mut world, 1.0);
        let z: Vec<f64> = world.placements(0).iter().map(|p| p.translate[2]).collect();
        assert!((z[0] - 1.0).abs() < 0.05 && (z[1] - z[0]).abs() < 1e-3 && z[2] == 0.0, "{z:?}");
        let mut world = space();
        for x in [0.0, 1.5, 4.0] {
            world.add(0, &at_x(x, 0.0)).unwrap();
        }
        world.set_fields(&[field(Falloff::Linear)]).unwrap();
        world.step(DT, 1);
        world.set_fields(&[]).unwrap();
        run(&mut world, 1.0);
        let z: Vec<f64> = world.placements(0).iter().map(|p| p.translate[2]).collect();
        assert!(z[0] > 0.0 && (z[1] / z[0] - 0.5).abs() < 0.02 && z[2] == 0.0, "half way out, half: {z:?}");
    }

    /// The mean distance of the bodies of group 0 from the origin.
    fn spread(world: &World) -> f64 {
        let p = world.placements(0);
        p.iter().map(|p| p.translate.iter().map(|v| v * v).sum::<f64>().sqrt()).sum::<f64>() / p.len() as f64
    }

    #[test]
    fn a_point_gathers_bodies_and_scatters_them_with_a_negative_strength() {
        let mut world = world_of(ball(), Settings { damping: 1.0, ..Settings::default() });
        for k in 0..12 {
            let (s, c) = (k as f32 * 30.0).to_radians().sin_cos();
            world.add(0, &Start { translate: [4.0 * c, 4.0 * s, 0.5], ..Start::default() }).unwrap();
        }
        run(&mut world, 1.0);
        assert!((spread(&world) - 4.03).abs() < 0.05, "{}", spread(&world));
        world.set_fields(&[Field { position: [0.0, 0.0, 0.5], ..point(8.0) }]).unwrap();
        run(&mut world, 5.0);
        let gathered = spread(&world);
        assert!(gathered < 2.2, "twelve balls of 1 m crowd round the point: {gathered}");
        world.set_fields(&[Field { position: [0.0, 0.0, 0.5], ..point(-8.0) }]).unwrap();
        run(&mut world, 3.0);
        assert!(spread(&world) > 8.0, "scattered: {}", spread(&world));
        // A body at the middle of the point is not thrown about: the field fades there.
        for off in [0.0, 0.05] {
            let mut world = space();
            world.add(0, &at_x(off, 0.0)).unwrap();
            world.set_fields(&[point(1000.0)]).unwrap();
            let mut farthest = 0.0f64;
            for _ in 0..120 {
                world.step(DT, 1);
                farthest = farthest.max(world.placements(0)[0].translate[0].abs());
            }
            assert!(farthest <= f64::from(off) * 1.5, "from {off} it went to {farthest}");
        }
    }

    #[test]
    fn a_vortex_drives_bodies_round_its_axis() {
        let mut world = space();
        world.add(0, &at_x(2.0, 0.0)).unwrap();
        world.add(0, &at_x(2.0, 7.0)).unwrap();
        let whirl = Field { kind: FieldKind::Vortex, strength: 60.0, ..Field::default() };
        world.set_fields(&[whirl]).unwrap();
        world.step(DT, 1);
        world.set_fields(&[]).unwrap();
        run(&mut world, 1.0);
        // About z, counterclockwise from above: at +x the push goes along +y, at any height.
        // One frame of 60 m/s^2 is 1 m/s.
        let p = world.placements(0);
        assert!((p[0].translate[1] - 1.0).abs() < 0.03 && p[0].translate[0] == 2.0, "{:?}", p[0].translate);
        assert_eq!(p[0].translate[1], p[1].translate[1]);
        // The other way round with a negative strength, and with the axis turned over.
        for turned in [Field { strength: -60.0, ..whirl }, Field { direction: [0.0, 0.0, -3.0], ..whirl }] {
            let mut world = space();
            world.add(0, &at_x(2.0, 0.0)).unwrap();
            world.kick(&Field { strength: 1.0, ..turned }).unwrap();
            world.kick(&turned).unwrap();
            run(&mut world, 1.0);
            assert!(world.placements(0)[0].translate[1] < -1.0);
        }
    }

    #[test]
    fn a_kick_acts_once() {
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.set_group(1, unit_box(), Material::default(), Mode::Fixed).unwrap();
        world.add(1, &at(9.0)).unwrap();
        world.kick(&wind(3.0)).unwrap();
        run(&mut world, 1.0);
        let x = world.placements(0)[0].translate[0];
        assert!((x - 3.0).abs() < 0.06, "3 m/s for a second: {x}");
        run(&mut world, 1.0);
        assert!(
            (world.placements(0)[0].translate[0] - 2.0 * x).abs() < 1e-3,
            "the speed stays, no more comes"
        );
        assert_eq!(world.placements(1)[0].translate, [0.0, 0.0, 9.0]);
        // A kick away from a point throws the bodies around it apart, and wakes those at rest.
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at_x(-1.0, 0.5)).unwrap();
        world.add(0, &at_x(1.0, 0.5)).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.asleep(), 2);
        world.kick(&Field { position: [0.0, 0.0, -0.5], ..point(-6.0) }).unwrap();
        assert_eq!(world.asleep(), 0);
        run(&mut world, 0.3);
        let p = world.placements(0);
        assert!(p[0].translate[0] < -1.5 && p[1].translate[0] > 1.5, "{:?} {:?}", p[0], p[1]);
        assert!(p[0].translate[2] > 0.6, "and up: {:?}", p[0]);
    }

    #[test]
    fn a_field_wakes_a_body_that_sleeps_and_keeps_it_awake() {
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(0.5)).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.asleep(), 1);
        // A wind too weak for the floor's grip: the box wakes and stays where it is.
        world.set_fields(&[wind(1.0)]).unwrap();
        world.step(DT, 1);
        assert_eq!(world.asleep(), 0, "a wind wakes it");
        run(&mut world, 4.0);
        assert_eq!(world.asleep(), 0);
        assert!(world.placements(0)[0].translate[0].abs() < 0.02);
        // A wind that grows little by little moves the box once it beats the grip.
        for k in 0..600 {
            world.set_fields(&[wind(1.0 + k as f32 * 0.02)]).unwrap();
            world.step(DT, 1);
        }
        assert!(world.placements(0)[0].translate[0] > 1.0, "13 m/s^2 beat the grip");
        // A push too weak for the solver to count the body as moving still moves it: the
        // body does not fall asleep on its way.
        let mut slow = space();
        slow.add(0, &at(0.0)).unwrap();
        slow.set_fields(&[wind(0.02)]).unwrap();
        run(&mut slow, 10.0);
        let x = slow.placements(0)[0].translate[0];
        assert!((x - 1.0).abs() < 0.05, "0.02 m/s^2 for ten seconds: {x}");
        // The wind gone, the box comes to rest and sleeps, and a push of its own wakes it.
        world.set_fields(&[]).unwrap();
        run(&mut world, 15.0);
        assert_eq!(world.asleep(), 1);
        world.set_pushes(0, &[Push { push: [0.0, 0.0, 30.0], ..Push::default() }]);
        run(&mut world, 0.5);
        assert!(world.placements(0)[0].translate[2] > 1.0, "lifted off");
    }

    #[test]
    fn each_body_takes_the_push_of_its_row() {
        let mut world = space();
        for k in 0..3 {
            world.add(0, &at_x(k as f32 * 3.0, 0.0)).unwrap();
        }
        // Too few rows: the last body is not pushed. A twist spins a body up.
        let up = |z: f32| Push { push: [0.0, 0.0, z], ..Push::default() };
        world.set_pushes(0, &[up(2.0), Push { twist: [0.0, 0.0, 90.0], ..up(-2.0) }]);
        run(&mut world, 1.0);
        let p = world.placements(0);
        assert!((p[0].translate[2] - 1.0).abs() < 0.05 && (p[1].translate[2] + 1.0).abs() < 0.05);
        assert_eq!(p[2].translate, [6.0, 0.0, 0.0]);
        assert!((p[1].rotate_degrees[2] - 45.0).abs() < 3.0, "90 degrees/s^2 for a second: {:?}", p[1]);
        assert_eq!(p[0].rotate_degrees, [0.0; 3]);
        // Too many rows: those beyond the bodies wait. A row that is not a number pushes
        // nothing, a huge one counts as the most there is.
        let odd = Push { push: [f32::NAN, f32::INFINITY, 1e30], twist: [f32::NAN; 3] };
        world.set_pushes(0, &[Push::default(), Push::default(), odd, up(5.0), up(5.0)]);
        world.step(DT, 1);
        world.set_pushes(0, &[]);
        run(&mut world, 1.0);
        let p = world.placements(0)[2].translate;
        assert!(sound(&world) && p[0] == 6.0 && p[1] == 0.0, "{p:?}");
        assert!((p[2] - f64::from(MAX_PUSH) / 60.0).abs() < 5.0, "one frame of the most: {p:?}");
        // A group that is not dynamic, and one that does not exist, take rows without harm.
        world.set_group(1, unit_box(), Material::default(), Mode::Fixed).unwrap();
        world.add(1, &at(50.0)).unwrap();
        world.set_pushes(1, &[up(9.0)]);
        world.set_targets(1, &[Some([0.0; 3])], Spring::default());
        world.set_pushes(7, &[up(9.0)]);
        world.set_targets(7, &[None], Spring::default());
        run(&mut world, 1.0);
        assert_eq!(world.placements(1)[0].translate, [0.0, 0.0, 50.0]);
    }

    #[test]
    fn a_spring_holds_a_body_at_its_target_against_gravity_and_lets_it_fall_at_pull_0() {
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(0.5)).unwrap();
        world.add(0, &at_x(5.0, 0.5)).unwrap();
        run(&mut world, 2.0);
        assert_eq!(world.asleep(), 2);
        // The first box is pulled 3 m up and 1 m aside. The second has no target and stays.
        let spring = Spring::default();
        world.set_targets(0, &[Some([1.0, 0.0, 3.5]), None], spring);
        run(&mut world, 4.0);
        let p = world.placements(0);
        let held = p[0].translate;
        assert!((held[0] - 1.0).abs() < 0.01 && (held[2] - 3.5).abs() < 0.01, "at its target: {held:?}");
        assert!((p[1].translate[2] - 0.5).abs() < 0.01 && (p[1].translate[0] - 5.0).abs() < 0.01);
        // With a damping of 1 it never swung past: checked on the way of a second box.
        world.set_targets(0, &[Some([1.0, 0.0, 3.5]), Some([5.0, 0.0, 4.0])], spring);
        let mut top = 0.0f64;
        for _ in 0..300 {
            world.step(DT, 1);
            top = top.max(world.placements(0)[1].translate[2]);
        }
        assert!(top > 3.95 && top < 4.01, "it rose to {top}");
        // Half a pull: half the gravity is left, and the spring gives way by about 9.81 / 2
        // over 40 / 2.
        world.set_targets(0, &[Some([1.0, 0.0, 3.5])], Spring { pull: 0.5, ..spring });
        run(&mut world, 5.0);
        let z = world.placements(0)[0].translate[2];
        assert!((z - (3.5 - 0.245)).abs() < 0.04, "{z}");
        assert!((world.placements(0)[1].translate[2] - 0.5).abs() < 0.03, "a body whose row went falls");
        // Let go, the box falls to the floor.
        world.set_targets(0, &[Some([1.0, 0.0, 3.5])], Spring { pull: 0.0, ..spring });
        run(&mut world, 3.0);
        let z = world.placements(0)[0].translate[2];
        assert!((z - 0.5).abs() < 0.02, "{z}");
        // So it does when it slept at its target and the targets go.
        world.set_targets(0, &[Some([1.0, 0.0, 3.5])], spring);
        run(&mut world, 8.0);
        assert!((world.placements(0)[0].translate[2] - 3.5).abs() < 0.01);
        assert_eq!(world.asleep(), 2, "held still, it sleeps in the air");
        world.set_targets(0, &[], spring);
        run(&mut world, 3.0);
        assert!((world.placements(0)[0].translate[2] - 0.5).abs() < 0.02);
    }

    #[test]
    fn a_spring_of_any_stiffness_brings_a_body_to_its_target() {
        for (stiffness, damping) in
            [(1.0, 1.0), (500.0, 0.2), (MAX_STIFFNESS, 0.0), (1e30, 5.0), (40.0, 100.0)]
        {
            let mut world = space();
            world
                .add(0, &Start { velocity: [0.0, 30.0, 0.0], spin: [0.0, 0.0, 720.0], ..Start::default() })
                .unwrap();
            world.set_targets(0, &[Some([10.0, 0.0, 0.0])], Spring { pull: 1.0, stiffness, damping });
            let mut farthest = 0.0f64;
            for _ in 0..600 {
                world.step(DT, 1);
                let p = world.placements(0)[0].translate;
                farthest = farthest.max(p.iter().map(|v| v.abs()).fold(0.0, f64::max));
            }
            assert!(
                farthest < 25.0,
                "stiffness {stiffness}, damping {damping}: it went as far as {farthest}"
            );
            let x = world.placements(0)[0].translate[0];
            let there = stiffness >= 40.0 && damping > 0.0 && damping < 50.0;
            assert!(!there || (x - 10.0).abs() < 0.05, "stiffness {stiffness}, damping {damping}: {x}");
        }
        // Numbers that are none let go instead of failing.
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        let odd = Spring { pull: f32::NAN, stiffness: f32::INFINITY, damping: -1.0 };
        world.set_targets(0, &[Some([f32::NAN, 0.0, 0.0])], Spring::default());
        world.set_targets(0, &[Some([1.0, 0.0, 0.0])], odd);
        run(&mut world, 1.0);
        assert_eq!(world.placements(0)[0].translate, [0.0; 3]);
    }

    #[test]
    fn bad_fields_are_errors_and_huge_ones_never_fling_a_body_out_of_reach() {
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.set_fields(&[wind(1.0)]).unwrap();
        for bad in [
            Field { strength: f32::NAN, ..wind(1.0) },
            Field { position: [0.0, f32::INFINITY, 0.0], ..wind(1.0) },
            Field { direction: [f32::NAN; 3], ..wind(1.0) },
            Field { radius: f32::NAN, ..wind(1.0) },
        ] {
            assert!(world.set_fields(&[wind(5.0), bad]).is_err());
            assert!(world.kick(&bad).is_err());
        }
        assert!(world.set_fields(&vec![wind(1.0); MAX_FIELDS + 1]).is_err());
        run(&mut world, 1.0);
        let x = world.placements(0)[0].translate[0];
        assert!((x - 0.5).abs() < 0.03, "the fields that failed changed nothing: {x}");
        // A wind without a direction blows nowhere.
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.set_fields(&[Field { direction: [0.0; 3], ..wind(50.0) }]).unwrap();
        run(&mut world, 1.0);
        assert_eq!(world.placements(0)[0].translate, [0.0; 3]);
        // The most there is of everything at once, for ten seconds: the body is fast and far,
        // and still where a number can say.
        let huge = Field { strength: f32::MAX, radius: f32::MAX, ..wind(1.0) };
        world.set_fields(&vec![huge; MAX_FIELDS]).unwrap();
        world.set_pushes(0, &[Push { push: [f32::MAX; 3], twist: [f32::MAX; 3] }]);
        world.kick(&Field { strength: f32::MAX, ..wind(1.0) }).unwrap();
        world.kick(&Field { position: [f32::MAX; 3], ..point(f32::MIN) }).unwrap();
        run(&mut world, 10.0);
        let p = world.placements(0)[0];
        let reach = f64::from(MAX_SPEED) * 10.0 * 1.01;
        assert!(sound(&world));
        assert!(p.translate[0] > 1000.0 && p.translate.iter().all(|v| v.abs() <= reach), "{p:?}");
        // It still answers: a spring fetches it back.
        world.set_fields(&[]).unwrap();
        world.set_pushes(0, &[]);
        world.set_targets(0, &[Some([0.0; 3])], Spring { stiffness: 2000.0, ..Spring::default() });
        run(&mut world, 20.0);
        let back = world.placements(0)[0].translate;
        assert!(back.iter().all(|v| v.abs() < 0.5), "{back:?}");
    }

    /// Whether the solver still moves everything: every body and obstacle is switched on, with
    /// a place and a speed that are numbers. `placements` cannot tell, since it reports 0 for
    /// what is not a number.
    fn sound(world: &World) -> bool {
        let bodies = world.world.rigid_bodies().all(|(_, b)| {
            b.is_enabled() && b.position().is_finite() && b.linvel().is_finite() && b.angvel().is_finite()
        });
        bodies && world.world.colliders.iter().all(|(_, c)| c.is_enabled()) && world.disabled() == 0
    }

    fn turned(degrees: [f32; 3]) -> Start {
        Start { rotate_degrees: degrees, ..Start::default() }
    }

    #[test]
    fn a_driven_body_whose_angle_wraps_takes_the_short_way() {
        let mut world = World::new(Settings { floor: None, ..Settings::default() });
        let slab = Shape::Box { center: [0.0; 3], half: [2.0, 2.0, 0.1] };
        world.set_group(0, unit_box(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(1, slab, Material::default(), Mode::Driven).unwrap();
        // Tilted by two degrees the other way: 358 is -2.
        world.add(1, &turned([0.0, 358.0, 0.0])).unwrap();
        world.add(0, &at_x(1.5, 0.7)).unwrap();
        world.add(0, &at_x(-1.5, 0.7)).unwrap();
        run(&mut world, 2.0);
        let before = world.placements(0);
        // Half a degree a frame, past 360 to 0 and on.
        for degrees in [358.5, 359.0, 359.5, 0.0, 0.5, 1.0] {
            world.place(1, &[turned([0.0, degrees, 0.0])], NEVER).unwrap();
            world.step(DT, 1);
        }
        for _ in 0..60 {
            world.step(DT, 1);
            for (now, was) in world.placements(0).iter().zip(&before) {
                let far = (0..3).map(|k| (now.translate[k] - was.translate[k]).powi(2)).sum::<f64>().sqrt();
                assert!(far < 0.3, "a box on the slab was flung {far} m, to {:?}", now.translate);
            }
        }
        // So with -180..180, as the angles come out of the solver, and with several steps.
        for substeps in [1, 3] {
            let mut world = box_at_rest_and(Mode::Driven);
            world.add(1, &Start { translate: [0.0, 0.0, 3.0], ..turned([0.0, 0.0, 179.0]) }).unwrap();
            for degrees in [179.5, 180.0, -179.5, -179.0] {
                let row = Start { translate: [0.0, 0.0, 3.0], ..turned([0.0, 0.0, degrees]) };
                world.place(1, &[row], NEVER).unwrap();
                world.step(DT, substeps);
                let spin = world.world.bodies.get(world.groups[1].as_ref().unwrap().bodies[0].body).unwrap();
                let fast = spin.angvel().length().to_degrees();
                assert!((fast - 30.0).abs() < 1.0, "half a degree a frame is 30 a second, not {fast}");
            }
        }
    }

    #[test]
    fn a_turn_too_far_for_one_frame_is_a_jump() {
        // A box at rest near the edge of a driven slab. Tilted by 30 degrees in one frame, the
        // slab throws the box up. Tilted by 120 degrees it is put there and throws nothing:
        // the box falls from where it was.
        let slab = Shape::Box { center: [0.0; 3], half: [2.0, 2.0, 0.1] };
        let on_a_slab = |x: f32| {
            let mut world = World::new(Settings { floor: None, ..Settings::default() });
            world.set_group(0, unit_box(), Material::default(), Mode::Dynamic).unwrap();
            world.set_group(1, slab.clone(), Material::default(), Mode::Driven).unwrap();
            world.add(1, &at(0.0)).unwrap();
            world.add(0, &at_x(x, 0.6)).unwrap();
            run(&mut world, 2.0);
            world
        };
        for (degrees, pushes) in [(120.0, false), (30.0, true)] {
            let mut world = on_a_slab(-1.5);
            world.place(1, &[turned([0.0, degrees, 0.0])], NEVER).unwrap();
            world.step(DT, 1);
            run(&mut world, 0.2);
            let p = world.placements(0)[0].translate;
            assert_eq!(p[2] > 1.0, pushes, "tilted by {degrees}: the box is at {p:?}");
            assert!(pushes || (p[2] < 0.6 && (p[0] + 1.5).abs() < 0.05), "{p:?}");
        }
        // With `jump` 0 nothing pushes, a turn on the spot included: the slab turns under the
        // box and does not take it along.
        let mut world = on_a_slab(1.5);
        for k in 1..=60 {
            world.place(1, &[turned([0.0, 0.0, k as f32])], 0.0).unwrap();
            world.step(DT, 1);
        }
        let p = world.placements(0)[0].translate;
        assert!((p[0] - 1.5).abs() < 1e-2 && p[1].abs() < 1e-2, "{p:?}");
        assert!((world.placements(1)[0].rotate_degrees[2] - 60.0).abs() < 1e-3);
    }

    #[test]
    fn an_obstacle_turned_past_a_quarter_takes_the_short_way() {
        // A sheet that stands upright in its own space, laid flat by a turn of 270 degrees
        // about x: the matrix of that turn gives its quaternion with the other sign from 270 on.
        let sheet = |degrees: f32| {
            let (s, c) = degrees.to_radians().sin_cos();
            Obstacle {
                vertices: vec![[-3.0, 0.0, -3.0], [3.0, 0.0, -3.0], [3.0, 0.0, 3.0], [-3.0, 0.0, 3.0]],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
                place: Place { translate: [0.0; 3], turn: [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]] },
            }
        };
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        world.set_obstacles(vec![sheet(268.0)]).unwrap();
        world.add(0, &Start { translate: [0.0, 1.5, 0.6], ..Start::default() }).unwrap();
        run(&mut world, 2.0);
        let before = world.placements(0)[0].translate;
        assert!((before[2] - 0.45).abs() < 0.1, "it rests on the sheet: {before:?}");
        for k in 1..=8 {
            world.move_obstacles(&[sheet(268.0 + 0.5 * k as f32).place], NEVER).unwrap();
            world.step(DT, 1);
            let sheet = world.obstacles[0].as_ref().unwrap().body;
            let spin = world.world.bodies.get(sheet).unwrap().angvel();
            // Half a degree a frame is 30 degrees a second about x.
            assert!((spin.x.to_degrees() - 30.0).abs() < 1.0, "step {k}: the sheet spins at {spin:?}");
        }
        for _ in 0..60 {
            world.step(DT, 1);
            let now = world.placements(0)[0].translate;
            let far = (0..3).map(|k| (now[k] - before[k]).powi(2)).sum::<f64>().sqrt();
            assert!(far < 0.3, "the box on the sheet was flung {far} m, to {now:?}");
        }
    }

    #[test]
    fn numbers_too_large_for_the_solver_are_brought_within_bounds() {
        // A scale whose volume no number holds: the body is as large as a body gets, and falls.
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &Start { translate: [0.0, 0.0, 5.0], scale: [1e15; 3], ..Start::default() }).unwrap();
        assert_eq!(world.placements(0)[0].scale, [MAX_SCALE; 3]);
        run(&mut world, 1.0);
        assert!(sound(&world));
        // It overlaps the floor by half its size and is pushed out, or still on its way up.
        assert!(world.placements(0)[0].translate[2] > 5.0, "{:?}", world.placements(0)[0]);
        // A speed and a spin beyond all measure: the fastest there is.
        let mut world = space();
        let wild = Start { velocity: [3e38, 0.0, 0.0], spin: [0.0, 0.0, 3e38], ..Start::default() };
        world.add(0, &wild).unwrap();
        run(&mut world, 1.0);
        assert!(sound(&world));
        // The solver has a limit of its own, and a lower one: 400 m/s.
        let x = world.placements(0)[0].translate[0];
        assert!((x - 400.0).abs() < 10.0, "a second at the fastest: {x}");
        // A place beyond the farthest is the farthest.
        let mut world = space();
        world.add(0, &at(3e38)).unwrap();
        world.step(DT, 1);
        assert!(sound(&world));
        assert_eq!(world.placements(0)[0].translate[2], f64::from(MAX_PLACE));
        // Shapes: the largest box, ball and offset at the largest scale and density still
        // weigh something a number holds. A hull that large is refused.
        let far = [3e38; 3];
        for shape in [Shape::Box { center: far, half: far }, Shape::Sphere { center: far, radius: 3e38 }] {
            let mut world = World::new(Settings::default());
            let heavy = Material { density: f32::MAX, ..Material::default() };
            world.set_group(0, shape, heavy, Mode::Dynamic).unwrap();
            world.add(0, &Start { scale: [f32::MAX; 3], ..Start::default() }).unwrap();
            world.set_group(1, unit_box(), Material::default(), Mode::Dynamic).unwrap();
            world.add(1, &at(-5.0)).unwrap();
            run(&mut world, 0.5);
            assert!(sound(&world));
        }
        let points = vec![[0.0; 3], [1e30, 0.0, 0.0], [0.0, 1e30, 0.0], [0.0, 0.0, 1e30]];
        assert!(check(Shape::Hull { points }).is_err());
        // An obstacle beyond the farthest place is refused, and so is one sent there.
        let mut world = world_of(unit_box(), Settings::default());
        let mut huge = slab(Place::default());
        huge.vertices[1] = [1e30, 0.0, 0.0];
        assert!(world.set_obstacles(vec![huge]).is_err());
        world.set_obstacles(vec![slab(Place::default())]).unwrap();
        world.move_obstacles(&[placed_at(1e30, 0.0)], NEVER).unwrap();
        world.step(DT, 1);
        assert!(sound(&world));
    }

    #[test]
    fn a_step_too_short_to_count_is_no_step() {
        // A box at rest on a slab. A time so short that its inverse is no number would give
        // every obstacle a speed that is none, and the solver would switch the slab off.
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        world.set_obstacles(vec![slab(placed_at(0.0, 0.0))]).unwrap();
        world.add(0, &at(0.5)).unwrap();
        run(&mut world, 2.0);
        for seconds in [1e-40, 1e-30, 1e-9, f32::MIN_POSITIVE] {
            world.step(seconds, 1);
            world.step(seconds, 8);
        }
        assert!(sound(&world));
        // The slab still holds the box: kicked sideways it slides and stays on it.
        world.kick(&wind(1.0)).unwrap();
        run(&mut world, 2.0);
        let p = world.placements(0)[0].translate;
        assert!(p[0] > 0.05 && (p[2] - 0.5).abs() < 0.02, "{p:?}");
        // A driven body is put at its row in such a step, as when time stands.
        let mut world = box_at_rest_and(Mode::Driven);
        world.add(1, &at_x(-3.0, 0.5)).unwrap();
        world.place(1, &[at_x(-2.8, 0.5)], NEVER).unwrap();
        world.step(1e-40, 1);
        assert!(sound(&world));
        assert!((world.placements(1)[0].translate[0] + 2.8).abs() < 1e-6);
    }

    #[test]
    fn a_body_the_solver_switched_off_is_counted() {
        let mut world = space();
        world.add(0, &at(0.0)).unwrap();
        world.add(0, &at(5.0)).unwrap();
        run(&mut world, 0.1);
        assert_eq!(world.disabled(), 0);
        // Nothing here can do it through the crate: reach past it.
        let handle = world.groups[0].as_ref().unwrap().bodies[1].body;
        world.world.bodies.get_mut(handle).unwrap().set_linvel(Vector::NAN, true);
        world.step(DT, 1);
        assert_eq!(world.disabled(), 1);
        assert_eq!(world.placements(0)[1].translate, [0.0, 0.0, 5.0], "it stays where it was");
    }

    #[test]
    fn a_negative_scale_mirrors_the_body_and_zero_is_the_smallest() {
        // A box of 1 m whose middle is 1 m over the body's origin. Mirrored along z the box
        // hangs under the origin, so the body comes to rest with its origin 1.5 m up.
        let shape = Shape::Box { center: [0.0, 0.0, 1.0], half: [0.5; 3] };
        let mut world = world_of(shape, Settings::default());
        let mirrored = Start { translate: [0.0, 0.0, 3.0], scale: [1.0, 1.0, -1.0], ..Start::default() };
        world.add(0, &mirrored).unwrap();
        let none = Start { translate: [5.0, 0.0, 3.0], scale: [0.0, -0.0, 1.0], ..Start::default() };
        world.add(0, &none).unwrap();
        run(&mut world, 3.0);
        let p = world.placements(0);
        assert_eq!(p[0].scale, [1.0, 1.0, -1.0], "the sign stays, for whoever draws the body");
        assert!((p[0].translate[2] - 1.5).abs() < 0.03, "{:?}", p[0].translate);
        assert_eq!(p[1].scale, [MIN_SCALE, MIN_SCALE, 1.0]);
        assert!(sound(&world));
    }

    #[test]
    fn a_body_that_appears_inside_a_sleeping_one_wakes_it() {
        for mode in [Mode::Fixed, Mode::Driven] {
            let mut world = box_at_rest_and(mode);
            // A new row: a box that overlaps the one at rest by 0.4 m.
            world.place(1, &[at_x(0.6, 0.5)], 1.0).unwrap();
            run(&mut world, 1.0);
            let x = world.placements(0)[0].translate[0];
            assert!(x < -0.39, "{mode:?}: the box at rest stayed inside the new one, at {x}");
            // So with a body that jumps there from far away, while time runs or stands.
            for seconds in [DT, 0.0] {
                let mut world = box_at_rest_and(mode);
                world.place(1, &[at_x(9.0, 0.5)], 1.0).unwrap();
                run(&mut world, 1.0);
                let resting = world.groups[0].as_ref().unwrap().bodies[0].body;
                assert!(world.world.bodies.get(resting).unwrap().is_sleeping());
                world.place(1, &[at_x(0.6, 0.5)], 1.0).unwrap();
                world.step(seconds, 1);
                run(&mut world, 1.0);
                let x = world.placements(0)[0].translate[0];
                assert!(x < -0.39, "{mode:?}: the box at rest stayed inside the one that came, at {x}");
            }
        }
    }

    #[test]
    fn a_fixed_body_that_moves_wakes_what_is_near_and_lets_the_rest_sleep() {
        // A box on a shelf with another box on top, and a heap of boxes far away.
        let mut world = world_of(unit_box(), Settings::default());
        world.set_group(1, unit_box(), Material::default(), Mode::Fixed).unwrap();
        world.set_group(2, unit_box(), Material::default(), Mode::Driven).unwrap();
        world.add(1, &at(3.0)).unwrap();
        world.add(2, &at_x(-30.0, 9.0)).unwrap();
        world.add(0, &at(4.0)).unwrap();
        world.add(0, &at(5.0)).unwrap();
        for k in 0..20 {
            world.add(0, &at_x(40.0 + 2.0 * k as f32, 0.5)).unwrap();
        }
        run(&mut world, 3.0);
        // Of the dynamic boxes; a fixed body counts as asleep too.
        let asleep = |world: &World| {
            let boxes = &world.groups[0].as_ref().unwrap().bodies;
            boxes.iter().filter(|b| world.world.bodies.get(b.body).unwrap().is_sleeping()).count()
        };
        assert_eq!(asleep(&world), 22);
        // The shelf changes its size a little, another joins it far off, the driven box jumps
        // about: the heap sleeps on.
        for k in 0..30 {
            let size = [1.0 + 0.01 * (k % 2) as f32; 3];
            let wobble = Start { translate: [0.0, 0.0, 3.0], scale: size, ..Start::default() };
            let rows: Vec<Start> = [wobble].into_iter().chain((k > 10).then(|| at_x(-60.0, 3.0))).collect();
            world.place(1, &rows, 1.0).unwrap();
            world.place(2, &[at_x(-30.0 + 5.0 * (k % 2) as f32, 9.0)], 1.0).unwrap();
            world.step(DT, 1);
            assert!(asleep(&world) >= 20, "frame {k}: {} of the heap's 20 sleep", asleep(&world));
        }
        // The shelf goes away: both boxes fall to the floor, the upper one too.
        world.place(1, &[at_x(-60.0, 3.0)], 1.0).unwrap();
        run(&mut world, 3.0);
        let p = world.placements(0);
        assert!(p[0].translate[2] < 2.0 && p[1].translate[2] < 2.0, "{:?} {:?}", p[0], p[1]);
        // So with an obstacle that jumps away from under a box, beside a heap.
        let mut world = world_of(unit_box(), Settings::default());
        world.set_obstacles(vec![slab(placed_at(0.0, 3.0))]).unwrap();
        world.add(0, &at(3.5)).unwrap();
        for k in 0..20 {
            world.add(0, &at_x(40.0 + 2.0 * k as f32, 0.5)).unwrap();
        }
        run(&mut world, 3.0);
        assert_eq!(world.asleep(), 21);
        world.move_obstacles(&[placed_at(-20.0, 3.0)], 1.0).unwrap();
        world.step(DT, 1);
        assert_eq!(world.asleep(), 20);
        run(&mut world, 3.0);
        assert!((world.placements(0)[0].translate[2] - 0.5).abs() < 0.03);
    }

    /// The pyramid of the hull tests: base 2 x 2 at z = 0, tip at z = 1.
    fn pyramid() -> Vec<[f32; 3]> {
        vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    }

    #[test]
    fn a_scale_that_flattens_a_hull_is_tried_once() {
        let mut world = World::new(Settings::default());
        world.set_group(0, Shape::Hull { points: pyramid() }, Material::default(), Mode::Driven).unwrap();
        world.add(0, &at(2.0)).unwrap();
        // A million times wider than high: no hull encloses room worth the name.
        let flat = Start { translate: [0.0, 0.0, 2.0], scale: [1000.0, 1000.0, 0.001], ..Start::default() };
        for _ in 0..3 {
            world.place(0, &[flat], 1.0).unwrap();
            world.step(DT, 1);
        }
        // The body takes the scale all the same, as the box around the flat pyramid.
        assert_eq!(world.placements(0)[0].scale, flat.scale);
        let hulls = &world.groups[0].as_ref().unwrap().hulls;
        let kept: Vec<&Made> =
            hulls.iter().filter(|(scale, _)| *scale == flat.scale).map(|(_, m)| m).collect();
        assert!(matches!(kept[..], [Made::Box(..)]), "kept once, so the hull is not tried again");
        // A ball dropped on it far from its middle comes to rest on it, 2 m up.
        world.set_group(1, ball(), Material::default(), Mode::Dynamic).unwrap();
        world.add(1, &at_x(400.0, 4.0)).unwrap();
        world.add(0, &Start { translate: [0.0, 5000.0, 2.0], ..flat }).unwrap();
        run(&mut world, 2.0);
        assert!((world.placements(1)[0].translate[2] - 2.5).abs() < 0.03, "{:?}", world.placements(1)[0]);
        assert!(sound(&world));
        // The smallest of everything still weighs enough for the solver.
        let dust = Material { density: 0.0, ..Material::default() };
        let mut world = World::new(Settings::default());
        world.set_group(0, Shape::Box { center: [0.0; 3], half: [0.0; 3] }, dust, Mode::Dynamic).unwrap();
        world.set_group(1, Shape::Sphere { center: [0.0; 3], radius: 0.0 }, dust, Mode::Dynamic).unwrap();
        world.set_group(2, Shape::Hull { points: pyramid() }, dust, Mode::Dynamic).unwrap();
        for group in 0..3 {
            let start = Start { translate: [group as f32, 0.0, 1.0], scale: [0.0; 3], ..Start::default() };
            world.add(group, &start).unwrap();
        }
        run(&mut world, 1.0);
        assert!(sound(&world));
        assert!(world.placements(2)[0].translate[2] < 0.01, "it fell: {:?}", world.placements(2)[0]);
    }

    #[test]
    fn a_scaled_hull_is_as_large_as_its_scale_says() {
        // The pyramid three times as wide and twice as high, and one as it is.
        let mut world = world_of(Shape::Hull { points: pyramid() }, Settings::default());
        world
            .add(0, &Start { translate: [0.0, 0.0, 1.0], scale: [3.0, 3.0, 2.0], ..Start::default() })
            .unwrap();
        world.add(0, &at_x(20.0, 1.0)).unwrap();
        // A ball dropped on the big pyramid's flank 2 m from its middle, where the small one
        // does not reach: it must not fall straight to the floor.
        world.set_group(1, ball(), Material::default(), Mode::Dynamic).unwrap();
        world.add(1, &at_x(2.0, 3.0)).unwrap();
        let mut lowest = f64::MAX;
        for _ in 0..90 {
            world.step(DT, 1);
            let b = world.placements(1)[0].translate;
            if b[0] < 2.3 {
                lowest = lowest.min(b[2]);
            }
        }
        // The flank at x = 2.3 is 2 * (1 - 2.3 / 3) = 0.47 m high; the ball's middle is 0.6 m
        // above it.
        assert!(lowest > 0.9, "the ball sank to {lowest} over the flank");
        let p = world.placements(0);
        assert_eq!(p[0].scale, [3.0, 3.0, 2.0]);
        assert!(p[0].translate[2].abs() < 0.05 && p[1].translate[2].abs() < 0.05, "on their bases");
        // A model drawn in millimetres and sized down by its row is a hull like any other.
        let large: Vec<[f32; 3]> = pyramid().iter().map(|p| p.map(|v| v * 2000.0)).collect();
        let mut world = world_of(Shape::Hull { points: large }, Settings::default());
        world.add(0, &Start { translate: [0.0, 0.0, 1.0], scale: [0.001; 3], ..Start::default() }).unwrap();
        run(&mut world, 3.0);
        assert!(world.placements(0)[0].translate[2].abs() < 0.03 && sound(&world));
        assert!(matches!(world.groups[0].as_ref().unwrap().hulls[..], [(_, Made::Hull(_))]));
    }

    #[test]
    fn hulls_of_points_on_a_plane_or_a_line_are_refused_and_large_clouds_are_thinned() {
        let flat = "the shape is flat";
        let plane: Vec<[f32; 3]> = (0..100).map(|k| [(k % 10) as f32, (k / 10) as f32, 2.0]).collect();
        assert!(check(Shape::Hull { points: plane }).unwrap_err().contains(flat));
        let line: Vec<[f32; 3]> = (0..50).map(|k| [k as f32, 2.0 * k as f32, -(k as f32)]).collect();
        assert!(check(Shape::Hull { points: line }).unwrap_err().contains(flat));
        // A ball of 10 000 points: every fifth is kept, and it still rolls like a ball.
        let cloud: Vec<[f32; 3]> = (0..10_000)
            .map(|k| {
                let (z, turn) = (1.0 - 2.0 * (k as f32 + 0.5) / 10_000.0, k as f32 * 2.399_963);
                let r = (1.0 - z * z).sqrt();
                [0.5 * r * turn.cos(), 0.5 * r * turn.sin(), 0.5 * z]
            })
            .collect();
        let Shape::Hull { points } = check(Shape::Hull { points: cloud }).unwrap() else { panic!() };
        assert_eq!(points.len(), 2000);
        assert!(points.len() <= MAX_HULL_POINTS);
        let mut world = world_of(Shape::Hull { points }, Settings::default());
        world.add(0, &at(3.0)).unwrap();
        run(&mut world, 3.0);
        assert!((world.placements(0)[0].translate[2] - 0.5).abs() < 0.02);
    }

    #[test]
    fn hostile_clouds_make_no_hull_panic() {
        // Numbers from a fixed sequence, so a failure can be found again.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let mut clouds: Vec<Vec<[f32; 3]>> = Vec::new();
        for round in 0..40 {
            let n = 4 + round * 7;
            // Nearly in one plane, nearly on one line, nearly in one place.
            clouds.push((0..n).map(|_| [next(), next(), next() * 1e-7]).collect());
            clouds.push((0..n).map(|_| [next(), next() * 1e-7, next() * 1e-7]).collect());
            clouds.push((0..n).map(|_| [1.0 + next() * 1e-7, next() * 1e-7, next() * 1e-7]).collect());
            // Many points twice, and a cube's corners many times over.
            let some: Vec<[f32; 3]> = (0..n).map(|_| [next(), next(), next()]).collect();
            clouds.push(some.iter().chain(&some).chain(&some).copied().collect());
            clouds.push(
                (0..n * 8).map(|k| [(k & 1) as f32, (k >> 1 & 1) as f32, (k >> 2 & 1) as f32]).collect(),
            );
            // Sizes that do not go together.
            let odd = |k: usize| [1e30, 1e-30, 1.0, 1e-7, 1e15][k % 5];
            clouds
                .push((0..n).map(|k| [next() * odd(k), next() * odd(k + 1), next() * odd(k + 2)]).collect());
            clouds.push((0..n).map(|k| [next() * odd(k), next() * odd(k), next() * odd(k)]).collect());
            clouds.push((0..n).map(|_| [next() * 1e-30, next() * 1e-30, next() * 1e-30]).collect());
            clouds.push((0..n).map(|_| [next() * 1e3, next() * 1e3, next() * 1e-4]).collect());
        }
        let scales = [[1.0; 3], [1e-3; 3], [1e3, 1e-3, 1.0], [1e3; 3], [1.0, 1e-3, 1e-3], [-1.0, 1.0, 1e3]];
        let (mut made, mut refused) = (0, 0);
        for cloud in clouds {
            let Ok(shape) = check(Shape::Hull { points: cloud }) else {
                refused += 1;
                continue;
            };
            made += 1;
            // Far apart and without a floor: this is about the hulls, and bodies of such
            // sizes inside each other are another matter.
            let mut world = world_of(shape, Settings { floor: None, ..Settings::default() });
            for scale in scales {
                let k = world.len() as f32;
                world
                    .add(0, &Start { translate: [k * 5000.0, 0.0, 0.0], scale, ..Start::default() })
                    .unwrap();
            }
            run(&mut world, 0.2);
            assert!(sound(&world), "{:?}", world.groups[0].as_ref().unwrap().shape);
        }
        assert!(made > 50 && refused > 50, "{made} hulls made, {refused} refused");
    }

    #[test]
    fn a_slow_body_in_free_flight_keeps_its_speed() {
        for speed in [0.05f32, 0.005] {
            let mut world = space();
            world.add(0, &Start { velocity: [speed, 0.0, 0.0], ..Start::default() }).unwrap();
            run(&mut world, 30.0);
            let x = world.placements(0)[0].translate[0];
            let gone = f64::from(speed) * 30.0;
            assert!((x - gone).abs() < 0.01 * gone, "{speed} m/s for 30 s: {x} m, {gone} m expected");
            let v =
                world.world.bodies.get(world.groups[0].as_ref().unwrap().bodies[0].body).unwrap().linvel();
            assert!((v.x - speed).abs() < 1e-4 * speed, "{speed} m/s became {v:?}");
        }
    }

    /// Each plane with its normal and two axes that lie in it.
    const PLANES: [(Plane, Vector, Vector, Vector); 3] = [
        (Plane::Xy, Vector::Z, Vector::X, Vector::Y),
        (Plane::Xz, Vector::Y, Vector::X, Vector::Z),
        (Plane::Yz, Vector::X, Vector::Y, Vector::Z),
    ];

    fn array(v: Vector) -> [f32; 3] {
        [v.x, v.y, v.z]
    }

    /// The solver's body for body `k` of `group`.
    fn body(world: &World, group: usize, k: usize) -> &RigidBody {
        world.world.bodies.get(world.groups[group].as_ref().unwrap().bodies[k].body).unwrap()
    }

    /// Where every dynamic body is and how it is turned, group after group.
    fn dynamic_poses(world: &World) -> Vec<Pose> {
        let groups = world.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic);
        groups.flat_map(|g| &g.bodies).map(|b| *world.world.bodies.get(b.body).unwrap().position()).collect()
    }

    /// Over all bodies: the farthest one has moved along `n` from `start` to `now`, in metres,
    /// and the most one has turned about an axis across `n`, in radians.
    fn out_of_plane(start: &[Pose], now: &[Pose], n: Vector) -> (f32, f32) {
        assert_eq!(start.len(), now.len());
        let (mut off, mut tilt) = (0.0f32, 0.0f32);
        for (a, b) in start.iter().zip(now) {
            off = off.max((b.translation - a.translation).dot(n).abs());
            // The turn since the start: its axis times the sine of half its angle.
            let turn = b.rotation * a.rotation.inverse();
            let axis = Vector::new(turn.x, turn.y, turn.z);
            tilt = tilt.max(2.0 * (axis - n * axis.dot(n)).length());
        }
        (off, tilt)
    }

    #[test]
    fn bodies_in_a_plane_stay_in_it_whatever_pushes_them() {
        for (plane, n, u, v) in PLANES {
            // A point given along u, v and n.
            let p = |a: f32, b: f32, c: f32| array(u * a + v * b + n * c);
            // Gravity pulls along v, and across the plane as well.
            let gravity = array(v * -9.81 + n * 4.0 + u);
            let mut world = World::new(Settings { gravity, plane, floor: None, ..Settings::default() });
            // A ground and two walls of fixed boxes, deep enough across the plane for two
            // layers of bodies: one at 0, one at 2 along n.
            let slab = |half: Vector| Shape::Box { center: [0.0; 3], half: array(half) };
            world.set_group(2, slab(u * 7.0 + v * 0.5 + n * 4.0), Material::default(), Mode::Fixed).unwrap();
            world.set_group(3, slab(u * 0.5 + v * 6.0 + n * 4.0), Material::default(), Mode::Fixed).unwrap();
            world.add(2, &Start { translate: p(0.0, -0.5, 1.0), ..Start::default() }).unwrap();
            for side in [-6.5, 6.5] {
                world.add(3, &Start { translate: p(side, 5.0, 1.0), ..Start::default() }).unwrap();
            }
            // Balls and boxes, the boxes tilted out of the plane at their start. A box of three
            // different sides that spins about another axis than its own would wobble, were
            // it free.
            let ball = Shape::Sphere { center: [0.0; 3], radius: 0.4 };
            world.set_group(0, ball, Material::default(), Mode::Dynamic).unwrap();
            let brick = Shape::Box { center: [0.0; 3], half: [0.5, 0.35, 0.2] };
            world.set_group(1, brick, Material::default(), Mode::Dynamic).unwrap();
            for k in 0..16 {
                let layer = if k % 3 == 0 { 2.0 } else { 0.0 };
                let at = p(-4.0 + (k % 8) as f32 * 1.1, 1.0 + (k / 8) as f32 * 1.5, layer);
                let tilt = if k >= 8 { [10.0, 20.0, 30.0] } else { [0.0; 3] };
                world.add(k / 8, &Start { translate: at, rotate_degrees: tilt, ..Start::default() }).unwrap();
            }
            let start = dynamic_poses(&world);
            run(&mut world, 3.0);
            // Fields: a wind and a pull that lean across the plane, a vortex about an axis in
            // it, and a kick from a point beside it.
            let field = |kind, strength, at: [f32; 3], toward: Vector| Field {
                kind,
                strength,
                position: at,
                direction: array(toward),
                ..Field::default()
            };
            world
                .set_fields(&[
                    field(FieldKind::Wind, 6.0, [0.0; 3], u + n * 2.0),
                    field(FieldKind::Point, 10.0, p(0.0, 3.0, 4.0), Vector::Z),
                    field(FieldKind::Vortex, 5.0, p(0.0, 2.0, 1.0), u),
                ])
                .unwrap();
            world.kick(&field(FieldKind::Point, -4.0, p(0.0, 0.0, -3.0), Vector::Z)).unwrap();
            run(&mut world, 3.0);
            world.set_fields(&[]).unwrap();
            // Pushes of their own, along and about every axis.
            let push = Push { push: array(n * 5.0 + u * 2.0), twist: [40.0, -60.0, 80.0] };
            world.set_pushes(0, &[push; 8]);
            world.set_pushes(1, &[push; 8]);
            run(&mut world, 3.0);
            world.set_pushes(0, &[]);
            world.set_pushes(1, &[]);
            // Springs to targets beside the plane.
            for group in [0, 1] {
                let targets: Vec<Option<[f32; 3]>> =
                    (0..8).map(|k| Some(array(body(&world, group, k).translation() + n * 2.0 + v))).collect();
                world.set_targets(group, &targets, Spring::default());
            }
            run(&mut world, 3.0);
            world.set_targets(0, &[], Spring::default());
            world.set_targets(1, &[], Spring::default());
            // A driven paddle sweeps through the heap, along u and across the plane at once.
            let paddle = Shape::Box { center: [0.0; 3], half: [0.5; 3] };
            world.set_group(4, paddle, Material::default(), Mode::Driven).unwrap();
            for k in 0..=480 {
                let s = k as f32 / 480.0;
                let row = Start { translate: p(-5.0 + 10.0 * s, 0.8, -1.0 + 4.0 * s), ..Start::default() };
                world.place(4, &[row], NEVER).unwrap();
                world.step(DT, 1);
            }
            let now = dynamic_poses(&world);
            let (off, tilt) = out_of_plane(&start, &now, n);
            assert!(off <= 1e-6, "{plane:?}: a body left its place across the plane by {off} m");
            assert!(tilt <= 1e-4, "{plane:?}: a body tilted out of the plane by {tilt} rad");
            let moved = start.iter().zip(&now).map(|(a, b)| (b.translation - a.translation).length());
            assert!(moved.fold(0.0f32, f32::max) > 1.0, "{plane:?}: the bodies were pushed about");
            assert!(sound(&world));
        }
    }

    /// The deepest any two bodies of `world` overlap, in metres; 0 when none do.
    fn deepest_overlap(world: &World) -> f32 {
        let colliders: Vec<&Collider> = world.world.colliders.iter().map(|(_, c)| c).collect();
        let mut deepest = 0.0f32;
        for (k, a) in colliders.iter().enumerate() {
            for b in &colliders[k + 1..] {
                let found =
                    rapier3d::parry::query::contact(a.position(), a.shape(), b.position(), b.shape(), 0.0);
                if let Ok(Some(contact)) = found {
                    deepest = deepest.max(-contact.dist);
                }
            }
        }
        deepest
    }

    #[test]
    fn bodies_at_different_depths_of_a_plane_touch_aslant_and_come_apart() {
        // Balls and boxes of 1 m in a row on the plane of x and y, each at its own depth in
        // z: 0, 0.1 or 0.3. Neighbours overlap in a picture taken from the front, and touch
        // with their rounded or tilted sides.
        let mut world =
            World::new(Settings { gravity: [0.0; 3], plane: Plane::Xy, floor: None, ..Settings::default() });
        world.set_group(0, ball(), Material::default(), Mode::Dynamic).unwrap();
        world.set_group(1, unit_box(), Material::default(), Mode::Dynamic).unwrap();
        let depths = [0.0, 0.1, 0.3];
        for k in 0..7 {
            let at = [-3.0 + k as f32, 0.2 * (k % 2) as f32, depths[k % 3]];
            world.add(k % 2, &Start { translate: at, ..Start::default() }).unwrap();
        }
        // A fixed wall on the right, a driven paddle on the left.
        let slab = Shape::Box { center: [0.0; 3], half: [0.5, 3.0, 2.0] };
        world.set_group(2, slab.clone(), Material::default(), Mode::Fixed).unwrap();
        world.set_group(3, slab, Material::default(), Mode::Driven).unwrap();
        world.add(2, &at_x(5.0, 0.0)).unwrap();
        world.add(3, &at_x(-5.0, 0.0)).unwrap();
        let start = dynamic_poses(&world);
        // The paddle squeezes the row against the wall in 4 s, holds 2 s, goes back in 2 s,
        // then the bodies are left alone for 12 s.
        let paddle = |s: f32| {
            let x = if s < 4.0 {
                -5.0 + 2.5 * s / 4.0
            } else if s < 6.0 {
                -2.5
            } else {
                -2.5 - 6.5 * ((s - 6.0) / 2.0).min(1.0)
            };
            at_x(x, 0.0)
        };
        let (mut fastest, mut squeezed) = (0.0f32, 0.0f32);
        for frame in 1..=1200 {
            let s = frame as f32 * DT;
            world.place(3, &[paddle(s)], NEVER).unwrap();
            world.step(DT, 1);
            let groups = world.groups.iter().flatten().filter(|g| g.mode == Mode::Dynamic);
            for b in groups.flat_map(|g| &g.bodies) {
                fastest = fastest.max(world.world.bodies.get(b.body).unwrap().linvel().length());
            }
            if s > 4.0 && s < 6.0 {
                squeezed = squeezed.max(deepest_overlap(&world));
            }
        }
        let now = dynamic_poses(&world);
        let (off, tilt) = out_of_plane(&start, &now, Vector::Z);
        let left = deepest_overlap(&world);
        assert!(sound(&world));
        assert!(off <= 1e-6 && tilt <= 1e-4, "{off} m, {tilt} rad");
        // The paddle reached the row: the first body was shoved, and the row was pressed
        // together (2 mm deep, on the day this was written).
        assert!(now[0].translation.x - start[0].translation.x > 0.5, "{:?}", now[0]);
        assert!(
            squeezed > 0.0 && squeezed < 0.02,
            "held against the wall, bodies overlapped by {squeezed} m"
        );
        assert!(fastest < 5.0, "the paddle goes 0.6 m/s, a body went {fastest} m/s");
        assert!(left < 5e-3, "two bodies still overlap by {left} m");
    }

    #[test]
    fn a_plane_switched_on_holds_bodies_where_they_are_and_switched_off_lets_them_go() {
        let mut world =
            world_of(unit_box(), Settings { gravity: [0.0; 3], floor: None, ..Settings::default() });
        world.add(0, &Start { velocity: [1.0; 3], spin: [90.0; 3], ..Start::default() }).unwrap();
        run(&mut world, 1.0);
        let before = *body(&world, 0, 0).position();
        assert!((before.translation.z - 1.0).abs() < 0.02, "{before:?}");
        world.apply(Settings { plane: Plane::Xy, ..world.settings() });
        let b = body(&world, 0, 0);
        assert_eq!((b.linvel().z, b.angvel().x, b.angvel().y), (0.0, 0.0, 0.0));
        assert_eq!((b.linvel().x, b.linvel().y), (1.0, 1.0), "the speed in the plane stays");
        assert!((b.angvel().z - 90f32.to_radians()).abs() < 1e-3, "{:?}", b.angvel());
        // Nothing starts over: the body goes on from where it was, and a wind across the plane
        // does not move it.
        world.set_fields(&[Field { kind: FieldKind::Wind, strength: 3.0, ..Field::default() }]).unwrap();
        run(&mut world, 1.0);
        let held = *body(&world, 0, 0).position();
        assert_eq!(held.translation.z, before.translation.z);
        assert!((held.translation.x - before.translation.x - 1.0).abs() < 0.03, "{held:?}");
        let (_, tilt) = out_of_plane(&[before], &[held], Vector::Z);
        assert!(tilt <= 1e-4, "{tilt}");
        // Off again, the wind lifts the body and a twist about x tilts it.
        world.apply(Settings { plane: Plane::Off, ..world.settings() });
        world.set_pushes(0, &[Push { twist: [90.0, 0.0, 0.0], ..Push::default() }]);
        run(&mut world, 1.0);
        let free = *body(&world, 0, 0).position();
        assert!(free.translation.z > before.translation.z + 1.0, "{free:?}");
        let (_, tilt) = out_of_plane(&[held], &[free], Vector::Z);
        assert!(tilt > 0.3, "{tilt}");
    }

    #[test]
    fn a_start_speed_across_the_plane_and_a_tilting_start_spin_are_dropped() {
        for (plane, n, u, v) in PLANES {
            let settings = Settings { gravity: [0.0; 3], floor: None, plane, ..Settings::default() };
            let mut world = world_of(unit_box(), settings);
            let start =
                Start { velocity: array(u + n * 5.0), spin: array((n + u + v) * 90.0), ..Start::default() };
            world.add(0, &start).unwrap();
            let b = body(&world, 0, 0);
            assert_eq!(b.linvel(), u, "{plane:?}");
            assert_eq!(b.angvel(), n * 90f32.to_radians(), "{plane:?}");
            run(&mut world, 1.0);
            let b = body(&world, 0, 0);
            assert_eq!(b.translation().dot(n), 0.0, "{plane:?}");
            assert!((b.translation().dot(u) - 1.0).abs() < 0.02, "{plane:?}: {:?}", b.translation());
            let (_, tilt) = out_of_plane(&[Pose::IDENTITY], &[*b.position()], n);
            assert!(tilt <= 1e-4, "{plane:?}: {tilt}");
        }
    }

    /// A world for balls on a wall: in the plane of x and y, without gravity, with nothing
    /// that slows a body and everything bouncing fully. Group 0 takes balls of 0.4 m; four
    /// fixed boxes frame the square from -5 to 5.
    fn wall() -> World {
        let settings = Settings {
            gravity: [0.0; 3],
            plane: Plane::Xy,
            floor: None,
            friction: 0.0,
            bounce: 1.0,
            damping: 0.0,
            spin_damping: 0.0,
        };
        let mut world = World::new(settings);
        let rubber = Material { friction: 0.0, bounce: 1.0, density: 1.0 };
        let ball = Shape::Sphere { center: [0.0; 3], radius: 0.2 };
        world.set_group(0, ball, rubber, Mode::Dynamic).unwrap();
        let long = Shape::Box { center: [0.0; 3], half: [6.0, 0.5, 1.0] };
        let tall = Shape::Box { center: [0.0; 3], half: [0.5, 6.0, 1.0] };
        world.set_group(1, long, rubber, Mode::Fixed).unwrap();
        world.set_group(2, tall, rubber, Mode::Fixed).unwrap();
        for side in [-5.5, 5.5] {
            world.add(1, &Start { translate: [0.0, side, 0.0], ..Start::default() }).unwrap();
            world.add(2, &Start { translate: [side, 0.0, 0.0], ..Start::default() }).unwrap();
        }
        world
    }

    #[test]
    fn balls_in_a_plane_between_boxes_keep_bouncing() {
        let mut world = wall();
        // Twelve balls at speeds from 5 cm/s to 2 m/s, each its own way.
        let speeds = [0.05f32, 0.1, 0.2, 0.3, 0.5, 0.7, 1.0, 1.2, 1.5, 1.7, 2.0, 0.05];
        for (k, speed) in speeds.iter().enumerate() {
            let (s, c) = (k as f32 * 47.0).to_radians().sin_cos();
            let at = [-4.0 + (k % 4) as f32 * 2.5, -3.0 + (k / 4) as f32 * 2.5, 0.0];
            world
                .add(0, &Start { translate: at, velocity: [speed * c, speed * s, 0.0], ..Start::default() })
                .unwrap();
        }
        let energy = |world: &World| {
            (0..speeds.len()).map(|k| body(world, 0, k).linvel().length_squared()).sum::<f32>()
        };
        let before = energy(&world);
        run(&mut world, 60.0);
        let after = energy(&world);
        let now: Vec<f32> = (0..speeds.len()).map(|k| body(&world, 0, k).linvel().length()).collect();
        let inside = (0..speeds.len()).all(|k| body(&world, 0, k).translation().abs().max_element() < 5.0);
        assert!(inside, "a ball left the frame");
        // The balls trade speed when they meet, so each has its own speed by now. Together they
        // keep nearly all of it: 98 % of the energy, on the day this was written.
        assert!(now.iter().all(|s| *s > 0.01), "every ball still moves: {now:?}");
        assert!(after > 0.95 * before, "energy kept: {after} of {before}");
        // A slow ball alone, started by a corner, meets both walls there and comes back.
        let mut world = wall();
        world
            .add(0, &Start { translate: [4.5, 4.5, 0.0], velocity: [0.05, 0.05, 0.0], ..Start::default() })
            .unwrap();
        run(&mut world, 60.0);
        let slow = body(&world, 0, 0);
        assert!(slow.translation().x < 4.5 && slow.translation().y < 4.5, "{slow:?}");
        let kept = slow.linvel().length() / 0.05f32.hypot(0.05);
        assert!((kept - 1.0).abs() < 0.01, "the slow ball keeps its speed: {kept}");
    }
}
