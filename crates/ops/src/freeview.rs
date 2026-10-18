//! The viewer's free view: a 3D node's scene seen from a camera of the viewer's own, which
//! changes no output and is not in the history. For a `tex.render` the scene is what it draws
//! (its geometry, texture, lights and look) with its camera drawn in as a frustum, beside the
//! projector lights and other lights; for a node with geometry out, the geometry lit as its
//! thumbnail is. The engine draws it on the cook thread after a pass, like the other pictures
//! the UI shows; the lines are drawn by the UI over the picture.

use penumbra_calib::unproject_clip;
use penumbra_core::{Error, NodeId};
use penumbra_gpu::geo::Mat4;
use penumbra_gpu::render::{Light, LightKind, Look, Renderer, Surface};
use penumbra_gpu::{Geo, Gpu, Lights, Tex, TexRing};
use penumbra_graph::{Cooker, DataKind, Network, NodeTypes};
use penumbra_mesh::Bounds;

use crate::geo::framing;
use crate::tex::render::{RENDER, Renders};
use crate::view::Orbit;

/// Largest side of the free view's picture.
pub const MAX_SIDE: u32 = 2048;
/// The field of view the free view starts with for geometry.
pub const FOV: f64 = 35.0;

/// What the viewer asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    pub node: NodeId,
    /// Which of the node's outputs the viewer shows.
    pub output: u32,
    /// The viewer's camera; `None` asks for the starting one ([`Picture::home`]).
    pub camera: Option<Orbit>,
    /// The picture's size in pixels (the viewer's).
    pub size: (u32, u32),
}

/// A line of the overlay in world space, with its sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub from: [f64; 3],
    pub to: [f64; 3],
    pub color: [u8; 3],
}

/// The free view as drawn.
#[derive(Clone)]
pub struct Picture {
    pub node: NodeId,
    pub output: u32,
    pub tex: Tex,
    /// The camera it was drawn with, and its world to clip matrix (to draw [`Picture::lines`]).
    pub camera: Orbit,
    pub clip_from_world: Mat4,
    /// Where the free view starts: the render's own camera, or the geometry framed.
    pub home: Orbit,
    /// Cameras, projectors and lights.
    pub lines: Vec<Line>,
}

/// How far towards the scene's middle cameras, projectors and spots are drawn: all the way,
/// their lines would cover what they look at.
const GIZMO_REACH: f64 = 0.3;

/// Colours of the overlay: cameras silver as their wires, lights warm.
const CAMERA_COLOR: [u8; 3] = [220, 220, 228];
const LIGHT_COLOR: [u8; 3] = [255, 200, 90];

/// Whether `node` has a free view when the viewer shows its output `output`: a render, or
/// geometry out.
pub fn available(net: &Network, types: &NodeTypes, node: NodeId, output: u32) -> bool {
    let Some(n) = net.node(node) else { return false };
    if n.kind == RENDER {
        return true;
    }
    types
        .desc(&n.kind)
        .and_then(|d| d.outputs.get(output as usize).or_else(|| d.outputs.first()))
        .is_some_and(|o| o.kind == DataKind::Geo)
}

/// Draws free views; one per viewer, owned by the cook thread.
#[derive(Default)]
pub struct FreeView {
    renderer: Renderer,
    ring: TexRing,
}

impl FreeView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn draw(
        &mut self,
        gpu: &Gpu,
        net: &Network,
        cooker: &Cooker,
        renders: &Renders,
        request: &Request,
    ) -> Result<Picture, Error> {
        let node = net.node(request.node).ok_or_else(|| Error::new("the node is gone"))?;
        let w = request.size.0.clamp(16, MAX_SIDE);
        let h = request.size.1.clamp(16, MAX_SIDE);
        let mut lines = Vec::new();
        let input = |i: usize| node.inputs.get(i).copied().flatten().and_then(|p| cooker.output(p));
        let (geo, texture, scene_lights, look, home) = if node.kind == RENDER {
            let scene =
                renders.scene(request.node).ok_or_else(|| Error::new("the render has not drawn yet"))?;
            let geo = input(0).and_then(|d| d.get::<Geo>()).ok_or_else(|| Error::new("no geometry yet"))?;
            let texture = input(1).and_then(|d| d.get::<Tex>());
            let lights: Vec<Light> =
                input(2).and_then(|d| d.get::<Lights>()).map(|l| l.0.clone()).unwrap_or_default();
            let bounds = geo.bounds();
            // The render's camera, drawn a part of the way to the scene's middle.
            let eye = scene.viewpoint.position();
            let focus = bounds.map_or(1.0, |b| distance(eye, center(&b)));
            if let Some(view) = scene.viewpoint.view(scene.size, |_| {}) {
                frustum(&view.clip_from_world, focus * GIZMO_REACH, CAMERA_COLOR, &mut lines);
            }
            light_lines(&lights, bounds, &mut lines);
            let home = Orbit::from_viewpoint(&scene.viewpoint, focus);
            (geo, texture, lights, scene.look, home)
        } else {
            let data = cooker
                .outputs(request.node)
                .and_then(|o| o.get(request.output as usize).or_else(|| o.first()))
                .and_then(|d| d.get::<Geo>())
                .ok_or_else(|| Error::new("no geometry yet"))?;
            let camera =
                framing(data.bounds(), (w, h), FOV).ok_or_else(|| Error::new("cannot frame the geometry"))?;
            let target = data.bounds().map_or([0.0; 3], |b| center(&b));
            let home = Orbit { eye: camera.position(), target, roll: 0.0, fov: FOV, ortho: None };
            let look = Look {
                surface: Surface::Material,
                shade_all: true,
                background: [0.0; 4],
                cull: false,
                antialias: true,
                overlay: None,
                shadow_size: 0,
                ao: None,
            };
            (data, None, Vec::new(), look, home)
        };
        let camera = request.camera.filter(Orbit::is_valid).unwrap_or(home);
        let (near, far) = depth_range(&camera, geo.bounds(), &lines);
        let viewpoint =
            camera.viewpoint(near, far).ok_or_else(|| Error::new("the view camera cannot be drawn"))?;
        let view =
            viewpoint.view((w, h), |_| {}).ok_or_else(|| Error::new("the view camera cannot be drawn"))?;
        // Geometry alone is lit as its thumbnail: a little from everywhere, the rest from the eye.
        let lights = if node.kind == RENDER {
            scene_lights
        } else {
            vec![Light::ambient([0.25; 3]), Light::point(view.eye, [0.75; 3])]
        };
        let tex = self.ring.next(gpu, w, h, "free view")?;
        self.renderer.render(gpu, geo, &lights, &view, &look, texture, &tex, None)?;
        Ok(Picture {
            node: request.node,
            output: request.output,
            tex,
            camera,
            clip_from_world: view.clip_from_world,
            home,
            lines,
        })
    }
}

fn center(b: &Bounds) -> [f64; 3] {
    b.center().map(f64::from)
}

/// `a + b * s`.
fn mad(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    mad(a, b, -1.0)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}

/// The diagonal of a bounding box.
fn diagonal(b: &Bounds) -> f64 {
    let s = b.size().map(f64::from);
    dot(s, s).sqrt()
}

/// Near and far for the free view: everything of the scene and its lines in range.
fn depth_range(camera: &Orbit, bounds: Option<Bounds>, lines: &[Line]) -> (f64, f64) {
    let mut far: f64 = camera.distance() * 2.0;
    if let Some(b) = bounds {
        far = far.max((distance(camera.eye, center(&b)) + 0.5 * diagonal(&b)) * 1.5);
    }
    for l in lines {
        far = far.max(distance(camera.eye, l.from) * 1.2).max(distance(camera.eye, l.to) * 1.2);
    }
    let far = if far.is_finite() && far > 1e-3 { far } else { 100.0 };
    (far * 1e-4, far)
}

/// A camera or projector as lines: from its lens to a rectangle `length` away (an orthographic
/// one as a box from its near rectangle), with a small triangle on the picture's top edge.
fn frustum(clip: &Mat4, length: f64, color: [u8; 3], out: &mut Vec<Line>) {
    let corner = |x: f64, y: f64| -> Option<([f64; 3], [f64; 3])> {
        let near = unproject_clip(clip, [x, y, 0.0])?;
        let mid = unproject_clip(clip, [x, y, 0.5])?;
        let d = sub(mid, near);
        let len = dot(d, d).sqrt();
        (len > 0.0).then(|| (near, d.map(|c| c / len)))
    };
    let Some(points) = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (0.0, 0.0)]
        .iter()
        .map(|(x, y)| corner(*x, *y))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let [bl, br, tr, tl, mid] = [0, 1, 2, 3, 4].map(|i| points.get(i).copied().unwrap_or_default());
    // An orthographic camera (w row 0 0 0 1): parallel rays, a box from its near rectangle.
    let parallel = clip.get(3).is_some_and(|r| r[0].abs() + r[1].abs() + r[2].abs() < 1e-12);
    let far = if parallel {
        let near = [bl, br, tr, tl].map(|(p, _)| p);
        let far = [bl, br, tr, tl].map(|(p, dir)| mad(p, dir, length));
        for (i, (n, f)) in near.iter().zip(&far).enumerate() {
            out.push(Line { from: *n, to: *f, color });
            out.push(Line { from: *f, to: far.get((i + 1) % 4).copied().unwrap_or(*f), color });
            out.push(Line { from: *n, to: near.get((i + 1) % 4).copied().unwrap_or(*n), color });
        }
        far
    } else {
        // The lens: where two opposite corners' rays meet.
        let apex = meeting(bl, tr).unwrap_or(mid.0);
        // The corners' rays are longer than the axis: scale so the rectangle stands at
        // `length` along the axis from the lens.
        let along = |(_, dir): ([f64; 3], [f64; 3])| mad(apex, dir, length / dot(dir, mid.1).max(0.05));
        let far = [bl, br, tr, tl].map(along);
        for (i, p) in far.iter().enumerate() {
            out.push(Line { from: apex, to: *p, color });
            out.push(Line { from: *p, to: far.get((i + 1) % 4).copied().unwrap_or(*p), color });
        }
        far
    };
    // The top: a triangle above the top edge, a fifth of the edge high.
    let [_, bottom_r, top_r, top_l] = far;
    let up = sub(top_r, bottom_r);
    let half = mad(top_r, sub(top_l, top_r), 0.5);
    let peak = mad(half, up, 0.2 * distance(top_r, top_l) / dot(up, up).sqrt().max(1e-12));
    out.push(Line { from: top_l, to: peak, color });
    out.push(Line { from: peak, to: top_r, color });
}

/// The point closest to both rays (start, unit direction); `None` for parallel rays.
fn meeting(a: ([f64; 3], [f64; 3]), b: ([f64; 3], [f64; 3])) -> Option<[f64; 3]> {
    let w = sub(a.0, b.0);
    let (bb, d, e) = (dot(a.1, b.1), dot(a.1, w), dot(b.1, w));
    let denom = 1.0 - bb * bb;
    if denom.abs() <= 1e-12 || denom.is_nan() {
        return None;
    }
    let p = mad(a.0, a.1, (bb * e - d) / denom);
    p.iter().all(|v| v.is_finite()).then_some(p)
}

/// The scene's lights as lines: projectors as frusta, spots as cones, points as small stars,
/// directional lights as an arrow onto the scene's middle.
fn light_lines(lights: &[Light], bounds: Option<Bounds>, out: &mut Vec<Line>) {
    let (mid, size) = match bounds {
        Some(b) => (center(&b), diagonal(&b).max(1e-3)),
        None => ([0.0; 3], 1.0),
    };
    let color = LIGHT_COLOR;
    for l in lights {
        match l.kind {
            LightKind::Projector => {
                if let Some(p) = &l.projector {
                    let length = (distance(l.position, mid) * GIZMO_REACH).max(size * 0.05);
                    frustum(&p.clip_from_world, length, color, out);
                }
            }
            LightKind::Spot => {
                let d = normalized(l.direction);
                let length = (distance(l.position, mid) * GIZMO_REACH).max(size * 0.05);
                let cos = f64::from(l.cone[0]).clamp(0.05, 1.0);
                let radius = length * (1.0 - cos * cos).sqrt() / cos;
                let (u, v) = perpendiculars(d);
                let end = mad(l.position, d, length);
                let ring: Vec<[f64; 3]> = (0..16)
                    .map(|i| {
                        let a = f64::from(i) / 16.0 * std::f64::consts::TAU;
                        mad(mad(end, u, radius * a.cos()), v, radius * a.sin())
                    })
                    .collect();
                for (i, p) in ring.iter().enumerate() {
                    out.push(Line {
                        from: *p,
                        to: ring.get((i + 1) % ring.len()).copied().unwrap_or(*p),
                        color,
                    });
                    if i % 4 == 0 {
                        out.push(Line { from: l.position, to: *p, color });
                    }
                }
            }
            LightKind::Point => {
                let r = size * 0.03;
                for axis in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                    out.push(Line { from: mad(l.position, axis, -r), to: mad(l.position, axis, r), color });
                }
            }
            LightKind::Directional => {
                let d = normalized(l.direction);
                let from = mad(mid, d, -size * 0.8);
                let to = mad(mid, d, -size * 0.5);
                out.push(Line { from, to, color });
                let (u, _) = perpendiculars(d);
                for s in [-1.0, 1.0] {
                    out.push(Line { from: to, to: mad(mad(to, d, -size * 0.06), u, size * 0.03 * s), color });
                }
            }
            LightKind::Ambient | LightKind::Environment => {}
        }
    }
}

fn normalized(v: [f64; 3]) -> [f64; 3] {
    let len = dot(v, v).sqrt();
    if len > 0.0 && len.is_finite() { v.map(|c| c / len) } else { [0.0, 0.0, -1.0] }
}

/// Two unit vectors perpendicular to `d` and to each other.
fn perpendiculars(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let helper = if d[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let u = normalized(cross(d, helper));
    (u, normalized(cross(d, u)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_calib::Camera;

    #[test]
    fn a_frustum_reaches_the_given_distance() {
        let camera = Camera {
            intrinsics: crate::geo::perspective((1920, 1080), 40.0, [0.0; 2]),
            pose: penumbra_calib::Pose::look_at([0.0, -5.0, 0.0], [0.0; 3], [0.0, 0.0, 1.0]).unwrap(),
        };
        let clip = camera.clip_from_world(0.1, 100.0).unwrap();
        let mut lines = Vec::new();
        frustum(&clip, 5.0, CAMERA_COLOR, &mut lines);
        // Four rays, four edges, the triangle's two sides.
        assert_eq!(lines.len(), 10);
        // The far rectangle stands at y = 0 (5 along the axis), as wide as 40 degrees high at
        // 16:9 says, and the triangle points up (+z).
        let far: Vec<[f64; 3]> = lines.iter().step_by(2).take(4).map(|l| l.to).collect();
        for p in &far {
            assert!(p[1].abs() < 1e-6, "{p:?}");
        }
        let half_h = 5.0 * 20f64.to_radians().tan();
        assert!(far.iter().any(|p| (p[2] - half_h).abs() < 1e-6));
        let peak = lines[8].to;
        assert!(peak[2] > half_h);
        // Every ray starts at the lens.
        assert!(lines.iter().step_by(2).take(4).all(|l| distance(l.from, [0.0, -5.0, 0.0]) < 1e-6));
    }
}
