//! Texture coordinates computed from where a mesh's points are: the picture thrown onto the
//! mesh along an axis, wrapped around it, or thrown onto each side of it.
//!
//! Coordinates are worked out per triangle. A triangle across the seam of a cylinder or sphere
//! would otherwise run through the whole picture backwards, and a corner on the axis has no
//! angle of its own. A vertex that needs two coordinates is copied, so triangles and parts stay
//! as they were. Coordinates may leave 0..1 near a seam: the renderer repeats the picture.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

use crate::{Bounds, Error, Mesh, Vertex};

/// How the picture reaches the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Planar,
    Cylinder,
    Sphere,
    /// Each triangle takes the side of a box it faces most.
    Cube,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    pub shape: Shape,
    /// Planar: the direction the picture is thrown along. Cylinder and sphere: the axis the
    /// picture wraps around, its top towards +axis. Unused by [`Shape::Cube`].
    pub axis: Axis,
    /// The box the picture spans. `None`: one scene unit per picture, centred on the origin.
    pub fit: Option<Bounds>,
    /// How many times the picture fits across, about its middle; negative mirrors.
    pub repeat: [f64; 2],
    /// Turns the picture on the surface, counter-clockwise as seen in the picture.
    pub rotate_degrees: f64,
    /// Moves the picture, in picture sizes: +x right, +y up.
    pub offset: [f64; 2],
}

/// A mesh's vertices and indices with the new coordinates. Ranges, parts and materials of the
/// mesh still apply: the triangles keep their order.
#[derive(Clone, Debug, PartialEq)]
pub struct Projected {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

type Point = [f64; 3];

/// Where `position` lands under `world` (row-major, for column vectors).
fn place(world: &[[f64; 4]; 4], p: [f32; 3]) -> Point {
    let row = |r: &[f64; 4]| r[0] * f64::from(p[0]) + r[1] * f64::from(p[1]) + r[2] * f64::from(p[2]) + r[3];
    [row(&world[0]), row(&world[1]), row(&world[2])]
}

/// The box of `mesh` under `world`: what a [`Projection::fit`] of that mesh spans.
pub fn placed_bounds(mesh: &Mesh, world: &[[f64; 4]; 4]) -> Option<Bounds> {
    Bounds::of(mesh.vertices.iter().map(|v| place(world, v.position).map(|c| c as f32)))
}

fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The picture's axes in the scene, as seen by someone facing the surface: its right, its top
/// and the direction towards the viewer. Right-handed, so nothing shows mirrored.
#[derive(Clone, Copy)]
struct Frame {
    right: Point,
    up: Point,
    front: Point,
}

const X: Point = [1.0, 0.0, 0.0];
const Y: Point = [0.0, 1.0, 0.0];
const Z: Point = [0.0, 0.0, 1.0];

const fn minus(p: Point) -> Point {
    [-p[0], -p[1], -p[2]]
}

const fn frame(right: Point, up: Point, front: Point) -> Frame {
    Frame { right, up, front }
}

/// Seen along `axis`, from the side a viewer usually stands: in front (-y) for y.
fn planar_frame(axis: Axis) -> Frame {
    match axis {
        Axis::X => frame(Y, Z, X),
        Axis::Y => frame(X, Z, minus(Y)),
        Axis::Z => frame(X, Y, Z),
    }
}

/// Wrapped around `axis`, the picture's middle towards `front`.
fn wrap_frame(axis: Axis) -> Frame {
    match axis {
        Axis::X => frame(minus(Z), X, minus(Y)),
        Axis::Y => frame(X, Y, Z),
        Axis::Z => frame(X, Z, minus(Y)),
    }
}

/// The side of a box that faces most the way `normal` does.
fn side_frame(normal: Point) -> Frame {
    let [x, y, z] = normal;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    if az >= ax && az >= ay {
        if z >= 0.0 { frame(X, Y, Z) } else { frame(X, minus(Y), minus(Z)) }
    } else if ay >= ax {
        if y >= 0.0 { frame(minus(X), Z, Y) } else { frame(X, Z, minus(Y)) }
    } else if x >= 0.0 {
        frame(Y, Z, X)
    } else {
        frame(minus(Y), Z, minus(X))
    }
}

/// A corner closer to the axis than this share of its distance has no usable angle.
const ON_AXIS: f64 = 1e-6;

/// Where the fit puts the picture.
struct Span {
    centre: Point,
    size: Point,
}

impl Span {
    fn new(fit: Option<Bounds>) -> Self {
        match fit {
            Some(b) => Span {
                centre: b.center().map(f64::from),
                // A mesh flat along an axis has no size there to divide by.
                size: b.size().map(|s| if s > 0.0 { f64::from(s) } else { 1.0 }),
            },
            None => Span { centre: [0.0; 3], size: [1.0; 3] },
        }
    }

    fn off_centre(&self, p: Point) -> Point {
        [p[0] - self.centre[0], p[1] - self.centre[1], p[2] - self.centre[2]]
    }

    /// `d` along the unit axis `axis`, in sizes of the span along that axis.
    fn along(&self, d: Point, axis: Point) -> f64 {
        dot(d, axis) / dot(self.size, axis.map(f64::abs))
    }

    fn planar(&self, p: Point, f: Frame) -> [f64; 2] {
        let d = self.off_centre(p);
        [0.5 + self.along(d, f.right), 0.5 - self.along(d, f.up)]
    }

    /// The angle around the frame's up axis as 0..1 with 0.5 at `front`, and `None` on the axis.
    fn turn(&self, d: Point, f: Frame) -> Option<f64> {
        let (right, front) = (dot(d, f.right), dot(d, f.front));
        let around = right.hypot(front);
        (around > ON_AXIS * dot(d, d).sqrt()).then(|| 0.5 + right.atan2(front) / TAU)
    }

    /// Height of a corner in the picture, 0 at the top.
    fn height(&self, shape: Shape, d: Point, f: Frame) -> f64 {
        if shape == Shape::Sphere {
            let length = dot(d, d).sqrt();
            return if length > 0.0 { (dot(d, f.up) / length).clamp(-1.0, 1.0).acos() / PI } else { 0.5 };
        }
        0.5 - self.along(d, f.up)
    }

    /// A wrapped triangle: its angles taken the short way round, and a corner on the axis at
    /// the mean of the others, so the triangle does not run off to one edge of the picture.
    fn wrapped(&self, shape: Shape, corners: [Point; 3], f: Frame) -> [[f64; 2]; 3] {
        let d = corners.map(|p| self.off_centre(p));
        let mut turns = d.map(|d| self.turn(d, f));
        let first = turns.iter().flatten().copied().next();
        if let Some(first) = first {
            for t in turns.iter_mut().flatten() {
                *t += (first - *t).round();
            }
        }
        let known: Vec<f64> = turns.iter().flatten().copied().collect();
        let mean = if known.is_empty() { 0.5 } else { known.iter().sum::<f64>() / known.len() as f64 };
        let corner = |k: Point, t: Option<f64>| [t.unwrap_or(mean), self.height(shape, k, f)];
        [corner(d[0], turns[0]), corner(d[1], turns[1]), corner(d[2], turns[2])]
    }
}

/// The picture's move, turn and repeat, applied to coordinates in the picture.
struct Adjust {
    offset: [f64; 2],
    sin: f64,
    cos: f64,
    repeat: [f64; 2],
}

impl Adjust {
    fn new(p: &Projection) -> Self {
        let (sin, cos) = p.rotate_degrees.to_radians().sin_cos();
        Adjust { offset: p.offset, sin, cos, repeat: p.repeat }
    }

    fn apply(&self, [u, v]: [f64; 2]) -> [f32; 2] {
        // Worked out with y up; the picture's v runs down.
        let (x, y) = (u - 0.5 - self.offset[0], 0.5 - v - self.offset[1]);
        let (a, b) = (self.cos * x + self.sin * y, self.cos * y - self.sin * x);
        let finite = |c: f64| if c.is_finite() { c as f32 } else { 0.0 };
        [finite(0.5 + a * self.repeat[0]), finite(0.5 - b * self.repeat[1])]
    }
}

/// The output vertices: each input vertex once, plus a copy for every other coordinate a
/// triangle asks of it.
struct Copies {
    vertices: Vec<Vertex>,
    first: Vec<Option<[u32; 2]>>,
    extra: HashMap<(u32, [u32; 2]), u32>,
    max: usize,
}

impl Copies {
    fn index(&mut self, i: u32, uv: [f32; 2]) -> Result<u32, Error> {
        let bits = uv.map(f32::to_bits);
        let missing = || Error::new("an index points past the vertices");
        let slot = self.first.get_mut(i as usize).ok_or_else(missing)?;
        match *slot {
            None => {
                *slot = Some(bits);
                self.vertices.get_mut(i as usize).ok_or_else(missing)?.uv = uv;
                Ok(i)
            }
            Some(b) if b == bits => Ok(i),
            Some(_) => {
                if let Some(copy) = self.extra.get(&(i, bits)) {
                    return Ok(*copy);
                }
                if self.vertices.len() >= self.max {
                    return Err(Error::new(format!("the seams need more than {} vertices", self.max)));
                }
                let copy = u32::try_from(self.vertices.len()).map_err(|_| Error::new("too many vertices"))?;
                let v = *self.vertices.get(i as usize).ok_or_else(missing)?;
                self.vertices.push(Vertex { uv, ..v });
                self.extra.insert((i, bits), copy);
                Ok(copy)
            }
        }
    }
}

/// `mesh` placed by `world`, with coordinates from `p`. At most `max_vertices` vertices.
pub fn project(
    mesh: &Mesh,
    world: &[[f64; 4]; 4],
    p: &Projection,
    max_vertices: usize,
) -> Result<Projected, Error> {
    let placed: Vec<Point> = mesh.vertices.iter().map(|v| place(world, v.position)).collect();
    let span = Span::new(p.fit);
    let adjust = Adjust::new(p);
    let mut copies = Copies {
        vertices: mesh.vertices.clone(),
        first: vec![None; mesh.vertices.len()],
        extra: HashMap::new(),
        max: max_vertices,
    };
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for corners in mesh.indices.as_chunks::<3>().0 {
        let at = corners.map(|i| placed.get(i as usize).copied());
        let [Some(a), Some(b), Some(c)] = at else {
            return Err(Error::new("an index points past the vertices"));
        };
        let uv = match p.shape {
            Shape::Planar => {
                let f = planar_frame(p.axis);
                [span.planar(a, f), span.planar(b, f), span.planar(c, f)]
            }
            Shape::Cube => {
                let f = side_frame(cross(sub(b, a), sub(c, a)));
                [span.planar(a, f), span.planar(b, f), span.planar(c, f)]
            }
            Shape::Cylinder | Shape::Sphere => span.wrapped(p.shape, [a, b, c], wrap_frame(p.axis)),
        };
        for (i, uv) in corners.iter().zip(uv) {
            indices.push(copies.index(*i, adjust.apply(uv))?);
        }
    }
    Ok(Projected { vertices: copies.vertices, indices })
}

fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: Point, b: Point) -> Point {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim;

    const IDENTITY: [[f64; 4]; 4] =
        [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

    fn projection(shape: Shape, axis: Axis, fit: Option<Bounds>) -> Projection {
        Projection { shape, axis, fit, repeat: [1.0; 2], rotate_degrees: 0.0, offset: [0.0; 2] }
    }

    fn mesh(points: &[[f32; 3]], indices: &[u32]) -> Mesh {
        Mesh {
            vertices: points.iter().map(|p| Vertex { position: *p, ..Vertex::default() }).collect(),
            indices: indices.to_vec(),
            ..Mesh::default()
        }
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5
    }

    fn uv_at(p: &Projected, position: [f32; 3]) -> Vec<[f32; 2]> {
        p.vertices.iter().filter(|v| v.position == position).map(|v| v.uv).collect()
    }

    /// A square in the xz plane from -1 to 1, as seen from the front.
    fn front_square() -> Mesh {
        mesh(&[[-1.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0]], &[0, 1, 2, 0, 2, 3])
    }

    fn fit_of(m: &Mesh) -> Option<Bounds> {
        placed_bounds(m, &IDENTITY)
    }

    #[test]
    fn planar_from_the_front_puts_the_picture_upright_and_unmirrored() {
        let m = front_square();
        let p = project(&m, &IDENTITY, &projection(Shape::Planar, Axis::Y, fit_of(&m)), 100).unwrap();
        assert_eq!(p.vertices.len(), 4);
        assert_eq!(p.indices, m.indices);
        let want = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (v, w) in p.vertices.iter().zip(want) {
            assert!(close(v.uv, w), "{:?} for {:?}", v.uv, v.position);
        }
    }

    #[test]
    fn units_put_one_picture_per_unit_around_the_origin() {
        let m = front_square();
        let p = project(&m, &IDENTITY, &projection(Shape::Planar, Axis::Y, None), 100).unwrap();
        assert!(close(p.vertices[1].uv, [1.5, -0.5]), "{:?}", p.vertices[1].uv);
    }

    #[test]
    fn the_projection_follows_the_placement() {
        // Turned a quarter about z: the square's +x end now points along +y.
        let turned =
            [[0.0, -1.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let m = front_square();
        let fit = placed_bounds(&m, &turned);
        let p = project(&m, &turned, &projection(Shape::Planar, Axis::X, fit), 100).unwrap();
        assert!(close(p.vertices[1].uv, [1.0, 0.0]), "{:?}", p.vertices[1].uv);
    }

    #[test]
    fn rotate_turns_the_picture_counter_clockwise() {
        // The picture's left edge comes to lie along the bottom of the surface.
        let m = front_square();
        let mut how = projection(Shape::Planar, Axis::Y, fit_of(&m));
        how.rotate_degrees = 90.0;
        let p = project(&m, &IDENTITY, &how, 100).unwrap();
        assert!(close(p.vertices[3].uv, [0.0, 0.0]), "bottom left {:?}", p.vertices[3].uv);
        assert!(close(p.vertices[2].uv, [0.0, 1.0]), "bottom right {:?}", p.vertices[2].uv);
    }

    #[test]
    fn offset_moves_the_picture_right_and_up() {
        let m = front_square();
        let mut how = projection(Shape::Planar, Axis::Y, fit_of(&m));
        how.offset = [0.25, 0.25];
        let p = project(&m, &IDENTITY, &how, 100).unwrap();
        // The top left of the surface now shows what lay a quarter left and above of it.
        assert!(close(p.vertices[0].uv, [-0.25, 0.25]), "{:?}", p.vertices[0].uv);
    }

    #[test]
    fn repeat_works_about_the_middle() {
        let m = front_square();
        let mut how = projection(Shape::Planar, Axis::Y, fit_of(&m));
        how.repeat = [2.0, 3.0];
        let p = project(&m, &IDENTITY, &how, 100).unwrap();
        assert!(close(p.vertices[0].uv, [-0.5, -1.0]), "{:?}", p.vertices[0].uv);
    }

    #[test]
    fn a_flat_mesh_does_not_divide_by_its_missing_depth() {
        let m = front_square();
        let p = project(&m, &IDENTITY, &projection(Shape::Planar, Axis::X, fit_of(&m)), 100).unwrap();
        assert!(p.vertices.iter().all(|v| v.uv.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn a_cylinder_faces_the_middle_of_the_picture_to_the_front() {
        let m = mesh(&[[0.0, -1.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, -1.0]], &[0, 1, 2]);
        let fit = Some(Bounds { min: [-1.0, -1.0, -1.0], max: [1.0, 1.0, 1.0] });
        let p = project(&m, &IDENTITY, &projection(Shape::Cylinder, Axis::Z, fit), 100).unwrap();
        assert!(close(p.vertices[0].uv, [0.5, 0.0]), "front top {:?}", p.vertices[0].uv);
        assert!(close(p.vertices[1].uv, [0.75, 0.5]), "right middle {:?}", p.vertices[1].uv);
        assert!(close(p.vertices[2].uv, [0.5, 1.0]), "front bottom {:?}", p.vertices[2].uv);
    }

    #[test]
    fn a_triangle_across_the_seam_takes_the_short_way_round() {
        // Two corners either side of the back (+y), a third further round.
        let m = mesh(&[[0.1, 1.0, 0.0], [-0.1, 1.0, 0.0], [-0.1, 1.0, 1.0]], &[0, 1, 2]);
        let p = project(&m, &IDENTITY, &projection(Shape::Cylinder, Axis::Z, None), 100).unwrap();
        let u: Vec<f32> = p.vertices.iter().map(|v| v.uv[0]).collect();
        let (lo, hi) = u.iter().fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
        assert!(hi - lo < 0.1, "{u:?}");
    }

    #[test]
    fn a_shared_vertex_on_the_seam_is_copied_for_each_side() {
        let m = seam_pair();
        let p = project(&m, &IDENTITY, &projection(Shape::Cylinder, Axis::Z, None), 100).unwrap();
        assert_eq!(p.indices.len(), 6);
        assert!(p.vertices.len() > 4, "{}", p.vertices.len());
        for t in p.indices.chunks(3) {
            let u: Vec<f32> = t.iter().map(|i| p.vertices[*i as usize].uv[0]).collect();
            let (lo, hi) = u.iter().fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
            assert!(hi - lo < 0.3, "{u:?}");
        }
    }

    #[test]
    fn a_corner_on_the_axis_takes_the_angle_of_its_triangle() {
        let m = mesh(&[[0.0, 0.0, 1.0], [-0.1, -1.0, 1.0], [0.1, -1.0, 1.0]], &[0, 1, 2]);
        let p = project(&m, &IDENTITY, &projection(Shape::Sphere, Axis::Z, None), 100).unwrap();
        assert!((p.vertices[0].uv[0] - 0.5).abs() < 1e-5, "{:?}", p.vertices[0].uv);
    }

    #[test]
    fn a_sphere_runs_from_the_top_of_the_picture_at_the_pole_to_the_bottom() {
        let m = prim::sphere(1.0, 24).unwrap();
        let p = project(&m, &IDENTITY, &projection(Shape::Sphere, Axis::Z, None), 100_000).unwrap();
        for v in &p.vertices {
            let want = (f64::from(v.position[2]).clamp(-1.0, 1.0).acos() / PI) as f32;
            assert!((v.uv[1] - want).abs() < 1e-3, "{:?}", v);
        }
        // No triangle stretches across the picture, the poles and the seam included.
        for t in p.indices.chunks(3) {
            let u: Vec<f32> = t.iter().map(|i| p.vertices[*i as usize].uv[0]).collect();
            let (lo, hi) = u.iter().fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
            assert!(hi - lo < 0.2, "{u:?}");
        }
    }

    #[test]
    fn each_side_of_a_box_shows_the_whole_picture_upright() {
        let m = prim::cuboid([2.0, 2.0, 2.0]).unwrap();
        let fit = fit_of(&m);
        let p = project(&m, &IDENTITY, &projection(Shape::Cube, Axis::Z, fit), 1000).unwrap();
        // From the front the top left corner is the picture's top left; a corner of the box
        // holds a different coordinate per side.
        let corner = uv_at(&p, [-1.0, -1.0, 1.0]);
        assert!(corner.iter().any(|uv| close(*uv, [0.0, 0.0])), "{corner:?}");
        assert!(corner.iter().any(|uv| close(*uv, [1.0, 0.0])), "{corner:?}");
        for uv in p.vertices.iter().map(|v| v.uv) {
            assert!((-1e-5..=1.00001).contains(&uv[0]) && (-1e-5..=1.00001).contains(&uv[1]), "{uv:?}");
        }
    }

    #[test]
    fn too_many_copies_fail_instead_of_growing_without_end() {
        // The second triangle starts on the far side of the seam, so its two shared corners
        // need copies.
        let m = seam_pair();
        let how = projection(Shape::Cylinder, Axis::Z, None);
        let e = project(&m, &IDENTITY, &how, 5).unwrap_err();
        assert!(e.0.contains("seams"), "{e:?}");
        assert_eq!(project(&m, &IDENTITY, &how, 6).unwrap().vertices.len(), 6);
    }

    /// Two triangles sharing an edge on the back (+y), one on either side of the seam.
    fn seam_pair() -> Mesh {
        mesh(&[[0.0, 1.0, 1.0], [0.0, 1.0, -1.0], [0.5, 0.8, 0.0], [-0.5, 0.8, 0.0]], &[0, 1, 2, 3, 0, 1])
    }

    #[test]
    fn a_broken_index_is_an_error() {
        let m = mesh(&[[0.0; 3]], &[0, 1, 2]);
        assert!(project(&m, &IDENTITY, &projection(Shape::Planar, Axis::Y, None), 10).is_err());
    }

    #[test]
    fn a_placement_that_is_not_finite_leaves_finite_coordinates() {
        let mut world = IDENTITY;
        world[0][0] = f64::NAN;
        let m = front_square();
        let p = project(&m, &world, &projection(Shape::Sphere, Axis::Z, None), 100).unwrap();
        assert!(p.vertices.iter().all(|v| v.uv.iter().all(|c| c.is_finite())));
    }
}
