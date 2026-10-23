//! Looking around in the viewer: the free view's camera (orbit, pan, dolly with the mouse;
//! flown with the 3D mouse), steering a camera node the same way, and taking the free view into
//! it. The free view itself is drawn by the engine ([`penumbra_ops::freeview`]); this module
//! keeps the viewer's cameras, turns input into camera moves and draws the picture's lines.
//!
//! The free view changes no output and is not in the history. Steering edits the camera node's
//! `position`, `target` and `roll` (and `size` when orthographic), one undo step per movement (a drag, a turn of the wheel, a
//! push of the 3D mouse until it rests again).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Response, Stroke, pos2};
use penumbra_core::{GroupId, NodeId, ParamPath, Value};
use penumbra_graph::{Network, NodeTypes, Op};
use penumbra_ops::freeview::{Line, Picture};
use penumbra_ops::tex::render::{CAMERA_INPUT, RENDER};
use penumbra_ops::view::{CAMERA, Orbit};
use penumbra_spacenav::Fly;

use crate::Action;

/// Degrees the camera turns per point dragged.
const TURN_PER_POINT: f64 = 0.4;
/// Dolly per point dragged with the right button, and per point of wheel.
const DOLLY_PER_POINT: f64 = 0.01;
const DOLLY_PER_WHEEL: f64 = 0.0015;
/// The 3D mouse at full deflection: distances to the target per second, degrees per second.
const FLY_SPEED: f64 = 1.5;
const FLY_TURN: f64 = 90.0;
/// A steering movement without input this long has ended (the wheel comes in ticks).
const GESTURE_END: Duration = Duration::from_millis(400);

/// What the viewer shows of a 3D node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The node's own output.
    Picture,
    /// The scene from the viewer's own camera.
    Free,
    /// The node's own output, and navigating moves its camera node.
    Steer,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Picture => "picture",
            Mode::Free => "free",
            Mode::Steer => "steer",
        }
    }
}

/// A camera move, from the mouse, the 3D mouse or a command.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Move {
    /// Degrees about the target: yaw (counter-clockwise from above), pitch (the eye rises).
    pub orbit: [f64; 2],
    /// Picture heights at the target's distance; the camera moves right and up.
    pub pan: [f64; 2],
    /// Distance factor (< 1 comes closer); 1 for none.
    pub dolly: f64,
    /// Flying: world units right, up, forward; degrees turned right and up, rolled right.
    pub travel: [f64; 3],
    pub look: [f64; 2],
    pub roll: f64,
}

impl Move {
    pub fn none() -> Self {
        Self { dolly: 1.0, ..Self::default() }
    }

    pub fn is_none(&self) -> bool {
        *self == Self::none()
    }

    pub fn apply(&self, o: &mut Orbit) {
        let before = *o;
        o.orbit(self.orbit[0], self.orbit[1]);
        o.pan(self.pan[0], self.pan[1]);
        o.dolly(self.dolly);
        o.travel(self.travel[0], self.travel[1], self.travel[2]);
        o.look(self.look[0], self.look[1]);
        o.roll = (o.roll + self.roll + 180.0).rem_euclid(360.0) - 180.0;
        if !o.is_valid() {
            *o = before;
        }
    }

    /// The 3D mouse's deflection held for `dt` seconds, at the camera's distance `distance`.
    pub fn fly(fly: Fly, dt: f64, distance: f64, horizon_locked: bool) -> Self {
        let speed = distance.max(1e-3) * FLY_SPEED * dt;
        let turn = FLY_TURN * dt;
        Self {
            travel: [f64::from(fly.right) * speed, f64::from(fly.up) * speed, f64::from(fly.forward) * speed],
            look: [f64::from(fly.yaw) * turn, f64::from(fly.pitch) * turn],
            roll: if horizon_locked { 0.0 } else { f64::from(fly.roll) * turn },
            ..Self::none()
        }
    }
}

/// The viewer's navigation state.
pub struct Navigation {
    /// The mode chosen per node; a node not listed shows its default ([`default_mode`]).
    modes: BTreeMap<NodeId, Mode>,
    /// The free view's camera per node; set from the first picture's starting camera.
    cameras: BTreeMap<NodeId, Orbit>,
    /// Size of the viewer's picture area in pixels, for the free view.
    pub size: (u32, u32),
    /// A steering movement under way: its undo group, the camera as steered so far, when the
    /// last input came.
    steering: Option<(GroupId, Orbit, Instant)>,
    /// The 3D mouse leaves roll alone until unlocked (its left button).
    pub horizon_locked: bool,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            modes: BTreeMap::new(),
            cameras: BTreeMap::new(),
            size: (960, 540),
            steering: None,
            horizon_locked: true,
        }
    }
}

/// What can be navigated about a node.
pub struct Scene {
    pub node: NodeId,
    pub render: bool,
    /// The camera node steering moves (the `view.camera` wired into the render), or why it
    /// cannot.
    pub steer: Result<NodeId, String>,
}

/// The 3D scene of `node` (shown at output `output`), if it has one.
pub fn scene(net: &Network, types: &NodeTypes, node: NodeId, output: u32) -> Option<Scene> {
    if !penumbra_ops::freeview::available(net, types, node, output) {
        return None;
    }
    let n = net.node(node)?;
    let render = n.kind == RENDER;
    let steer = if render { steer_target(net, types, node) } else { Err("geometry has no camera".into()) };
    Some(Scene { node, render, steer })
}

fn steer_target(net: &Network, types: &NodeTypes, render: NodeId) -> Result<NodeId, String> {
    let n = net.node(render).ok_or("the node is gone")?;
    let port = n
        .inputs
        .get(CAMERA_INPUT)
        .copied()
        .flatten()
        .ok_or("the render has no camera: wire a view.camera into its `camera` input")?;
    let source = net.node(port.node).ok_or("the camera input's node is gone")?;
    if source.kind != CAMERA {
        return Err(format!("the camera comes from a {}; only a view.camera can be steered", source.kind));
    }
    let camera = port.node;
    let calibrated = net
        .node(camera)
        .and_then(|c| c.param(types.desc(&c.kind)?, "calibration"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if calibrated {
        return Err(
            "the camera is calibrated (its `calibration` param); steering would replace the calibration"
                .into(),
        );
    }
    Ok(camera)
}

/// The camera params of `node` as stored.
pub fn stored_orbit(net: &Network, types: &NodeTypes, node: NodeId) -> Option<Orbit> {
    let n = net.node(node)?;
    let desc = types.desc(&n.kind)?;
    let three = |name: &str| match n.param(desc, name)?.components()? {
        &[x, y, z] => Some([x, y, z]),
        _ => None,
    };
    let ortho = n.param(desc, "projection").and_then(Value::as_str) == Some("orthographic");
    let o = Orbit {
        eye: three("position")?,
        target: three("target")?,
        roll: n.param(desc, "roll")?.as_f64()?,
        fov: n.param(desc, "fov")?.as_f64()?,
        ortho: if ortho { Some(n.param(desc, "size")?.as_f64()?) } else { None },
    };
    o.is_valid().then_some(o)
}

/// The edits that give camera node `node` the camera `o`; `fov` too when `with_fov`; an
/// orthographic camera's `size` always (dollying changes it).
pub fn orbit_ops(net: &Network, node: NodeId, o: &Orbit, with_fov: bool) -> Vec<Op> {
    let Some(path) = net.path_of(node) else { return Vec::new() };
    let param = |name: &str| format!("{path}.{name}").parse::<ParamPath>().ok();
    let round = |v: f64| (v * 1e6).round() / 1e6;
    let mut values = vec![
        ("position", Value::Vec3(o.eye.map(round))),
        ("target", Value::Vec3(o.target.map(round))),
        ("roll", Value::Float(round(o.roll))),
    ];
    match o.ortho {
        Some(h) => values.push(("size", Value::Float(round(h)))),
        None if with_fov => values.push(("fov", Value::Float(round(o.fov)))),
        None => {}
    }
    values.into_iter().filter_map(|(name, value)| Some(Op::Set { param: param(name)?, value })).collect()
}

/// The default mode: geometry shows the free view (its own picture is a fixed thumbnail), a
/// render its picture.
pub fn default_mode(scene: &Scene) -> Mode {
    if scene.render { Mode::Picture } else { Mode::Free }
}

impl Navigation {
    pub fn mode(&self, scene: &Scene) -> Mode {
        let m = self.modes.get(&scene.node).copied().unwrap_or_else(|| default_mode(scene));
        match m {
            Mode::Steer if scene.steer.is_err() => Mode::Picture,
            Mode::Picture if !scene.render => Mode::Free,
            m => m,
        }
    }

    pub fn set_mode(&mut self, node: NodeId, mode: Mode) {
        self.modes.insert(node, mode);
        self.steering = None;
    }

    pub fn camera(&self, node: NodeId) -> Option<Orbit> {
        self.cameras.get(&node).copied()
    }

    pub fn set_camera(&mut self, node: NodeId, o: Orbit) {
        if o.is_valid() {
            self.cameras.insert(node, o);
        }
    }

    /// Back to where the free view starts (the next picture brings it).
    pub fn home(&mut self, node: NodeId) {
        self.cameras.remove(&node);
    }

    /// A picture arrived: its starting camera becomes the free view's, if it has none yet.
    pub fn arrived(&mut self, picture: &Picture) {
        self.cameras.entry(picture.node).or_insert(picture.home);
    }

    /// Applies `m` in the mode the viewer is in: the free view's camera, or the steered camera
    /// node (edits into `actions`, grouped per movement). Returns a message when it cannot.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        m: &Move,
        scene: &Scene,
        net: &Network,
        types: &NodeTypes,
        can_edit: bool,
        new_group: &dyn Fn() -> GroupId,
        now: Instant,
        actions: &mut Vec<Action>,
    ) -> Result<(), String> {
        if m.is_none() {
            return Ok(());
        }
        match self.mode(scene) {
            Mode::Picture => Ok(()),
            Mode::Free => {
                if let Some(o) = self.cameras.get_mut(&scene.node) {
                    m.apply(o);
                }
                Ok(())
            }
            Mode::Steer => {
                if !can_edit {
                    return Err("Claude is editing; take over to steer".into());
                }
                let target = scene.steer.clone()?;
                let (group, mut orbit) = match self.steering.take() {
                    Some((g, o, _)) => (g, o),
                    None => (
                        new_group(),
                        stored_orbit(net, types, target).ok_or("the camera's numbers cannot be read")?,
                    ),
                };
                m.apply(&mut orbit);
                actions.push(Action::Edit {
                    ops: orbit_ops(net, target, &orbit, false),
                    label: "steer the camera".into(),
                    group: Some(group),
                });
                self.steering = Some((group, orbit, now));
                Ok(())
            }
        }
    }

    /// Ends a steering movement that has had no input for a while.
    pub fn settle(&mut self, now: Instant, moving: bool) {
        if !moving && self.steering.is_some_and(|(_, _, t)| now.saturating_duration_since(t) > GESTURE_END) {
            self.steering = None;
        }
    }

    /// The edits that take the free view into the scene's camera node (with its field of view).
    pub fn take_view(&self, scene: &Scene, net: &Network) -> Result<Vec<Op>, String> {
        let target = scene.steer.clone()?;
        let o = self.camera(scene.node).ok_or("the free view has no camera yet")?;
        Ok(orbit_ops(net, target, &o, true))
    }
}

/// The mouse on the viewer as a move: left drag orbits, middle drag (or shift and left) pans,
/// right drag and the wheel dolly. `height` is the picture's height in points.
pub fn mouse_move(ui: &egui::Ui, response: &Response, height: f32) -> Move {
    let mut m = Move::none();
    let delta = response.drag_delta();
    let h = f64::from(height.max(1.0));
    let (dx, dy) = (f64::from(delta.x), f64::from(delta.y));
    let shift = ui.input(|i| i.modifiers.shift);
    if response.dragged_by(egui::PointerButton::Middle)
        || (shift && response.dragged_by(egui::PointerButton::Primary))
    {
        // The scene follows the pointer: the camera moves the other way.
        m.pan = [-dx / h, dy / h];
    } else if response.dragged_by(egui::PointerButton::Primary) {
        m.orbit = [-dx * TURN_PER_POINT, dy * TURN_PER_POINT];
    } else if response.dragged_by(egui::PointerButton::Secondary) {
        m.dolly = (dy * DOLLY_PER_POINT).exp();
    }
    if response.hovered() {
        let scroll = f64::from(ui.input(|i| i.smooth_scroll_delta.y));
        if scroll != 0.0 {
            m.dolly *= (-scroll * DOLLY_PER_WHEEL).exp();
        }
    }
    m
}

/// Draws the picture's lines (cameras, projectors, lights) over `rect`, where the picture is
/// shown whole.
pub fn draw_lines(painter: &egui::Painter, rect: Rect, picture: &Picture) {
    let m = picture.clip_from_world;
    let clip = |p: [f64; 3]| -> [f64; 4] {
        std::array::from_fn(|r| {
            let row = m.get(r).copied().unwrap_or([0.0; 4]);
            row[0] * p[0] + row[1] * p[1] + row[2] * p[2] + row[3]
        })
    };
    let to_screen = |c: [f64; 4]| -> Pos2 {
        let (x, y) = (c[0] / c[3], c[1] / c[3]);
        pos2(
            rect.min.x + ((x + 1.0) * 0.5) as f32 * rect.width(),
            rect.min.y + ((1.0 - y) * 0.5) as f32 * rect.height(),
        )
    };
    // Only what lies in front of the camera: segments cut where they cross its plane.
    const NEAR_W: f64 = 1e-6;
    for Line { from, to, color } in &picture.lines {
        let (mut a, mut b) = (clip(*from), clip(*to));
        if a[3] < NEAR_W && b[3] < NEAR_W {
            continue;
        }
        if a[3] < NEAR_W || b[3] < NEAR_W {
            let t = (NEAR_W - a[3]) / (b[3] - a[3]);
            let mut cut = a;
            for (c, (x, y)) in cut.iter_mut().zip(a.iter().zip(&b)) {
                *c = x + (y - x) * t;
            }
            if a[3] < NEAR_W {
                a = cut;
            } else {
                b = cut;
            }
        }
        let (p, q) = (to_screen(a), to_screen(b));
        if [p, q].iter().all(|v| v.x.is_finite() && v.y.is_finite()) {
            let [r, g, bl] = *color;
            painter.line_segment([p, q], Stroke::new(1.25, Color32::from_rgb(r, g, bl)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flying_respects_the_horizon_lock() {
        let fly = Fly { right: 1.0, up: 0.0, forward: 0.5, pitch: 0.0, yaw: 1.0, roll: 1.0 };
        let m = Move::fly(fly, 0.1, 2.0, true);
        assert_eq!(m.roll, 0.0);
        assert!((m.travel[0] - 0.3).abs() < 1e-9 && (m.travel[2] - 0.15).abs() < 1e-9);
        assert!((m.look[0] - 9.0).abs() < 1e-9);
        let m = Move::fly(fly, 0.1, 2.0, false);
        assert!((m.roll - 9.0).abs() < 1e-9);
        let mut o = Orbit { eye: [0.0, -4.0, 0.0], target: [0.0; 3], roll: 170.0, fov: 40.0, ortho: None };
        Move { roll: 20.0, ..Move::none() }.apply(&mut o);
        assert!((o.roll + 170.0).abs() < 1e-9, "wraps into -180..180: {}", o.roll);
        assert!(Move::none().is_none());
    }
}
