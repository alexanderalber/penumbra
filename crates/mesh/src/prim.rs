//! Primitives: a box, a UV sphere, a plane (whole or split into a grid). Each is one part named
//! after it, with normals and texture coordinates (origin top left). World z is up; the front is
//! -y (seen from a viewer standing at -y looking towards +y).

use crate::{Builder, Error, Limits, Mesh, Vertex};

/// Most segments around a sphere.
pub const MAX_SEGMENTS: u32 = 1024;

/// A box centred on the origin, `size` along x, y, z. Each face has the whole texture: the
/// sides upright (z up), going round like a label; the top seen from above with its upper edge
/// at +y, the bottom seen from below.
pub fn cuboid(size: [f32; 3]) -> Result<Mesh, Error> {
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("box")?;
    let h = size.map(|s| 0.5 * s);
    // (normal, right, up) per face, seen from outside; right x up = normal.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
    ];
    for (n, r, u) in faces {
        let corner = |s: f32, t: f32| -> [f32; 3] {
            [
                (n[0] + s * r[0] + t * u[0]) * h[0],
                (n[1] + s * r[1] + t * u[1]) * h[1],
                (n[2] + s * r[2] + t * u[2]) * h[2],
            ]
        };
        let mut ids = [0u32; 4];
        for (id, (s, t)) in ids.iter_mut().zip([(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)]) {
            *id = b.vertex(Vertex {
                position: corner(s, t),
                normal: n,
                uv: [0.5 * (s + 1.0), 0.5 * (1.0 - t)],
            })?;
        }
        let [a, c1, c2, d] = ids;
        // Counter-clockwise seen from outside: top left, bottom left, bottom right.
        b.triangle([a, d, c2], part, 0)?;
        b.triangle([a, c2, c1], part, 0)?;
    }
    Ok(b.finish())
}

/// A UV sphere around the origin: `segments` around (at least 3), half as many rings. The
/// texture wraps around once, its top at the north pole (+z), its middle (u = 0.5) facing -y.
pub fn sphere(radius: f32, segments: u32) -> Result<Mesh, Error> {
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("sphere")?;
    let seg = segments.clamp(3, MAX_SEGMENTS);
    let rings = (seg / 2).max(2);
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let theta = v * std::f32::consts::PI;
        for s in 0..=seg {
            let u = s as f32 / seg as f32;
            let phi = u * std::f32::consts::TAU;
            // u = 0 faces +y, going round towards +x (counter-clockwise seen from outside).
            let n = [-theta.sin() * phi.sin(), theta.sin() * phi.cos(), theta.cos()];
            b.vertex(Vertex { position: n.map(|c| c * radius), normal: n, uv: [u, v] })?;
        }
    }
    let row = seg + 1;
    for r in 0..rings {
        for s in 0..seg {
            let a = r * row + s;
            let c = a + row;
            if r > 0 {
                b.triangle([a, c, a + 1], part, 0)?;
            }
            if r + 1 < rings {
                b.triangle([a + 1, c, c + 1], part, 0)?;
            }
        }
    }
    Ok(b.finish())
}

/// An upright rectangle in the xz plane facing -y, `size[0]` wide (x) and `size[1]` high (z),
/// centred on the origin: a wall seen from the front, the texture upright.
pub fn plane(size: [f32; 2]) -> Result<Mesh, Error> {
    grid(size, 1)
}

/// Most cells along each side of a [`grid`].
pub const MAX_CELLS: u32 = 1024;

/// [`plane`] split into `cells` by `cells` quads (1 to [`MAX_CELLS`]), the texture over the
/// whole. Each quad's diagonal is marked as inside its polygon, so a wireframe draws the grid's
/// lines, not its triangles.
pub fn grid(size: [f32; 2], cells: u32) -> Result<Mesh, Error> {
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("plane")?;
    let [w, h] = size;
    let n = cells.clamp(1, MAX_CELLS);
    let normal = [0.0, -1.0, 0.0];
    let row = n + 1;
    for j in 0..=n {
        let v = j as f32 / n as f32;
        for i in 0..=n {
            let u = i as f32 / n as f32;
            b.vertex(Vertex { position: [(u - 0.5) * w, 0.0, (0.5 - v) * h], normal, uv: [u, v] })?;
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = j * row + i;
            let (c1, d) = (a + 1, a + row);
            let c2 = d + 1;
            // Counter-clockwise seen from the front; the diagonal a-c2 lies opposite d, then c1.
            b.triangle([a, d, c2], part, 0)?;
            b.inner_edges(0b010);
            b.triangle([a, c2, c1], part, 0)?;
            b.inner_edges(0b100);
        }
    }
    Ok(b.finish())
}

/// `mesh` with every position moved by `offset` (for building test scenes from primitives).
pub fn moved(mut mesh: Mesh, offset: [f32; 3]) -> Mesh {
    for v in &mut mesh.vertices {
        v.position = [v.position[0] + offset[0], v.position[1] + offset[1], v.position[2] + offset[2]];
    }
    mesh
}
