//! `view.camera`: one camera as data, wired into `tex.render`'s `camera` input (and later into a
//! projector light), so a single node says where several others see from and a calibrated projector
//! is kept in one place. `tex.render` has the same params for when nothing is wired.
//!
//! The camera is a pinhole as `calib` models projectors: built from the params (position,
//! target, field of view, roll, lens shift) or a calibrated `calib::Camera` as JSON (what
//! `calib.solve` returns). On the wire it does not know the picture's size yet; the render
//! fits it to its own ([`Viewpoint::camera`]).

use penumbra_calib::{Camera, Intrinsics, Pose};
use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use crate::geo::{UP_AXIS, perspective};

pub const CAMERA: &str = "view.camera";

/// The params that make a camera: on `tex.render` they count only while its camera input is
/// not connected.
pub const CAMERA_PARAMS: [&str; 8] =
    ["position", "target", "fov", "roll", "shift", "calibration", "near", "far"];

/// A camera on a `view` connection, for a picture of any size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewpoint {
    pub pose: Pose,
    pub lens: Lens,
    /// Nearest and farthest distance drawn.
    pub near: f64,
    pub far: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lens {
    /// Vertical field of view in degrees, lens shift in picture sizes (positive: the picture
    /// moves right and up); fits any picture.
    Fov { degrees: f64, shift: [f64; 2] },
    /// A calibrated lens in the pixels of the picture it was calibrated for; scaled to others.
    Calibrated(Intrinsics),
}

impl Viewpoint {
    /// The camera the params describe (see [`params`] and [`more_params`]).
    pub fn from_params(cx: &CookContext<'_>) -> Result<Self, CookError> {
        let (near, far) = (cx.float("near")?, cx.float("far")?);
        if !(near > 0.0 && far > near) {
            return Err(CookError::failed("`far` must be beyond `near`"));
        }
        let json = cx.text("calibration")?.trim();
        if !json.is_empty() {
            let c: Camera = serde_json::from_str(json)
                .map_err(|e| CookError::failed(format!("`calibration` is not a camera calibration: {e}")))?;
            if c.intrinsics.size.contains(&0) {
                return Err(CookError::failed("`calibration` has no picture size"));
            }
            return Ok(Self { pose: c.pose, lens: Lens::Calibrated(c.intrinsics), near, far });
        }
        let v = |name: &str| {
            cx.param(name)
                .and_then(|v| v.components())
                .ok_or_else(|| CookError::failed(format!("`{name}` must be numbers")))
        };
        let three = |name: &str| match v(name)? {
            &[x, y, z] => Ok([x, y, z]),
            _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
        };
        let shift = match v("shift")? {
            &[x, y] => [x, y],
            _ => [0.0; 2],
        };
        let (eye, target) = (three("position")?, three("target")?);
        // Straight up or down, the picture's top points to +y.
        let pose = Pose::look_at(eye, target, UP_AXIS)
            .or_else(|| Pose::look_at(eye, target, [0.0, 1.0, 0.0]))
            .ok_or_else(|| CookError::failed("`position` and `target` are the same point"))?
            .rolled(cx.float("roll")?);
        Ok(Self { pose, lens: Lens::Fov { degrees: cx.float("fov")?, shift }, near, far })
    }

    /// The camera for a picture of `size`; what does not fit goes to `warn`.
    pub fn camera(&self, size: (u32, u32), warn: impl Fn(String)) -> Camera {
        let intrinsics = match self.lens {
            Lens::Fov { degrees, shift } => perspective(size, degrees, shift),
            Lens::Calibrated(mut c) => {
                // The calibration's pixels scaled to this picture's.
                let [w0, h0] = c.size.map(|v| f64::from(v.max(1)));
                let (sx, sy) = (f64::from(size.0) / w0, f64::from(size.1) / h0);
                if (sx / sy - 1.0).abs() > 0.01 {
                    warn(format!(
                        "the camera was calibrated for {}x{}, another aspect ratio than {}x{}",
                        c.size[0], c.size[1], size.0, size.1
                    ));
                }
                if c.distortion != [0.0; 2] {
                    warn("lens distortion is not rendered".into());
                }
                c.focal = [c.focal[0] * sx, c.focal[1] * sy];
                c.principal = [c.principal[0] * sx, c.principal[1] * sy];
                c.size = [size.0, size.1];
                c
            }
        };
        Camera { intrinsics, pose: self.pose }
    }

    /// Where the camera is; what the pose's rounding leaves of a zero is zero.
    pub fn position(&self) -> [f64; 3] {
        let p = Camera { intrinsics: perspective((1, 1), 40.0, [0.0; 2]), pose: self.pose }.position();
        let scale = p.iter().fold(1.0f64, |m, v| m.max(v.abs()));
        p.map(|v| if v.abs() < 1e-12 * scale { 0.0 } else { v })
    }

    /// The vertical field of view in degrees (a calibrated lens's at its centre).
    pub fn fov(&self) -> f64 {
        match self.lens {
            Lens::Fov { degrees, .. } => degrees,
            Lens::Calibrated(c) => {
                (2.0 * (0.5 * f64::from(c.size[1]) / c.focal[1].abs().max(1e-9)).atan()).to_degrees()
            }
        }
    }
}

/// The camera's main params: `position`, `target`, `fov`, `roll`.
pub fn params(desc: NodeDesc) -> NodeDesc {
    desc.param(
        ParamSpec::vec3("position", [2.4, -3.2, 1.6]).range(-10.0, 10.0).doc("Where the camera is (z up)"),
    )
    .param(ParamSpec::vec3("target", [0.0; 3]).range(-10.0, 10.0).doc("What it looks at"))
    .param(
        ParamSpec::float("fov", 40.0)
            .clamp(1.0, 170.0)
            .range(5.0, 120.0)
            .unit(Unit::Degrees)
            .doc("Vertical field of view"),
    )
    .param(
        ParamSpec::float("roll", 0.0)
            .clamp(-360.0, 360.0)
            .range(-180.0, 180.0)
            .unit(Unit::Degrees)
            .doc("Turn about the view axis; positive tilts the picture's top to the right"),
    )
}

/// The camera's rarely used params, in `group`: `shift`, `calibration`, `near`, `far`.
pub fn more_params(desc: NodeDesc, group: &str) -> NodeDesc {
    desc.param(
        ParamSpec::vec2("shift", [0.0; 2])
            .range(-1.0, 1.0)
            .group(group)
            .doc("Lens shift in picture sizes: [0, 0.5] moves the picture up by half its height"),
    )
    .param(ParamSpec::text("calibration", "").group(group).doc(
        "A camera calibration as JSON (the `camera` that calib.solve returns); when set, it replaces position, target, fov, roll and shift",
    ))
    .param(
        ParamSpec::float("near", 0.05)
            .clamp(1e-4, 1e4)
            .curve(Curve::Log)
            .group(group)
            .doc("Nearest distance drawn"),
    )
    .param(
        ParamSpec::float("far", 100.0)
            .clamp(1e-3, 1e6)
            .curve(Curve::Log)
            .group(group)
            .doc("Farthest distance drawn"),
    )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = more_params(
        params(NodeDesc::new(
            CAMERA,
            "A camera: where it is, what it looks at, its lens; for tex.render's camera input",
        )),
        penumbra_core::MORE,
    )
    .output("out", DataKind::View);
    types.register(FnNodeType::new(desc, || Box::new(CameraNode) as Box<dyn Operator>))
}

struct CameraNode;

impl Operator for CameraNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        Ok(Cooked::Outputs(vec![Data::new(DataKind::View, Viewpoint::from_params(cx)?)]))
    }
}

/// A camera as one flies it: where it is, the point it looks at (which orbiting turns about),
/// its roll and field of view. The viewer's own camera, and what steering a camera node edits
/// (`position`, `target`, `roll`, `fov` are the camera params). Pure math, so the commands, the
/// mouse and the SpaceMouse all move it alike.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Orbit {
    pub eye: [f64; 3],
    pub target: [f64; 3],
    /// Degrees; positive tilts the picture's top to the right (as the `roll` param).
    pub roll: f64,
    /// Vertical field of view in degrees.
    pub fov: f64,
}

/// Highest and lowest elevation orbiting and looking reach: straight up or down the picture's
/// top would be undefined.
const MAX_ELEVATION: f64 = 89.0;
/// Nearest the eye comes to the target.
const MIN_DISTANCE: f64 = 1e-4;

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add_scaled(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

fn length(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// Azimuth (degrees from +x towards +y), elevation (degrees above the xy plane) and length.
fn spherical(v: [f64; 3]) -> (f64, f64, f64) {
    let len = length(v);
    if len.is_nan() || len <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    (v[1].atan2(v[0]).to_degrees(), (v[2] / len).clamp(-1.0, 1.0).asin().to_degrees(), len)
}

fn from_spherical(az: f64, el: f64, len: f64) -> [f64; 3] {
    let (az, el) = (az.to_radians(), el.to_radians());
    [len * el.cos() * az.cos(), len * el.cos() * az.sin(), len * el.sin()]
}

impl Orbit {
    /// The pose, as `view.camera` makes it from the same numbers.
    pub fn pose(&self) -> Option<Pose> {
        Pose::look_at(self.eye, self.target, UP_AXIS)
            .or_else(|| Pose::look_at(self.eye, self.target, [0.0, 1.0, 0.0]))
            .map(|p| p.rolled(self.roll))
    }

    pub fn viewpoint(&self, near: f64, far: f64) -> Option<Viewpoint> {
        Some(Viewpoint {
            pose: self.pose()?,
            lens: Lens::Fov { degrees: self.fov, shift: [0.0; 2] },
            near,
            far,
        })
    }

    /// The orbit that sees as `v` does, looking at a point `focus` ahead.
    pub fn from_viewpoint(v: &Viewpoint, focus: f64) -> Self {
        let eye = v.position();
        let [_, _, forward] = v.pose.rotation_matrix();
        let target = add_scaled(eye, forward, focus.max(MIN_DISTANCE));
        let level = Orbit { eye, target, roll: 0.0, fov: v.fov() };
        // The roll is the turn from the level camera's right to this one's.
        let roll = match level.pose() {
            Some(p) => {
                let [x0, y0, _] = p.rotation_matrix();
                let [x, _, _] = v.pose.rotation_matrix();
                let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                dot(x, y0).atan2(dot(x, x0)).to_degrees()
            }
            None => 0.0,
        };
        Orbit { roll, ..level }
    }

    pub fn distance(&self) -> f64 {
        length(sub(self.eye, self.target))
    }

    /// The camera's right, up and forward in the world (unit length).
    pub fn axes(&self) -> Option<([f64; 3], [f64; 3], [f64; 3])> {
        let [x, y, z] = self.pose()?.rotation_matrix();
        Some((x, [-y[0], -y[1], -y[2]], z))
    }

    /// Turns the eye about the target: `yaw` about the world's up (positive: counter-clockwise
    /// seen from above), `pitch` up and down (positive: the eye rises), in degrees.
    pub fn orbit(&mut self, yaw: f64, pitch: f64) {
        let (az, el, len) = spherical(sub(self.eye, self.target));
        let el = (el + pitch).clamp(-MAX_ELEVATION, MAX_ELEVATION);
        self.eye = add_scaled(self.target, from_spherical(az + yaw, el, len.max(MIN_DISTANCE)), 1.0);
    }

    /// Turns the view about the eye (flying): `yaw` positive turns right, `pitch` positive
    /// looks up, in degrees.
    pub fn look(&mut self, yaw: f64, pitch: f64) {
        let (az, el, len) = spherical(sub(self.target, self.eye));
        let el = (el + pitch).clamp(-MAX_ELEVATION, MAX_ELEVATION);
        self.target = add_scaled(self.eye, from_spherical(az - yaw, el, len.max(MIN_DISTANCE)), 1.0);
    }

    /// Moves eye and target together in the camera's frame, in world units.
    pub fn travel(&mut self, right: f64, up: f64, forward: f64) {
        let Some((r, u, f)) = self.axes() else { return };
        for (axis, amount) in [(r, right), (u, up), (f, forward)] {
            self.eye = add_scaled(self.eye, axis, amount);
            self.target = add_scaled(self.target, axis, amount);
        }
    }

    /// [`Orbit::travel`] sideways in picture heights at the target's distance: (1, 0) moves the
    /// camera right by as much as the picture shows from top to bottom there.
    pub fn pan(&mut self, right: f64, up: f64) {
        let height = 2.0 * self.distance() * (0.5 * self.fov.to_radians()).tan();
        self.travel(right * height, up * height, 0.0);
    }

    /// Moves the eye towards the target (`factor` < 1) or away from it.
    pub fn dolly(&mut self, factor: f64) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        let offset = sub(self.eye, self.target);
        let len = length(offset);
        if len.is_nan() || len <= 0.0 {
            return;
        }
        let new = (len * factor).max(MIN_DISTANCE);
        self.eye = add_scaled(self.target, offset, new / len);
    }

    /// All numbers finite and the eye apart from the target.
    pub fn is_valid(&self) -> bool {
        self.eye.iter().chain(&self.target).chain([&self.roll, &self.fov]).all(|v| v.is_finite())
            && self.distance() > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9)
    }

    #[test]
    fn an_orbit_round_trips_through_a_viewpoint() {
        let o = Orbit { eye: [2.0, -3.0, 1.5], target: [0.2, 0.1, 0.4], roll: 17.0, fov: 40.0 };
        let v = o.viewpoint(0.1, 100.0).unwrap();
        let back = Orbit::from_viewpoint(&v, o.distance());
        assert!(close(back.eye, o.eye) && close(back.target, o.target), "{back:?}");
        assert!((back.roll - o.roll).abs() < 1e-9 && (back.fov - o.fov).abs() < 1e-9);
    }

    #[test]
    fn orbiting_keeps_the_target_and_distance() {
        let mut o = Orbit { eye: [0.0, -4.0, 0.0], target: [0.0; 3], roll: 0.0, fov: 40.0 };
        o.orbit(90.0, 0.0);
        // Counter-clockwise from above: from -y to +x.
        assert!(close(o.eye, [4.0, 0.0, 0.0]), "{:?}", o.eye);
        o.orbit(0.0, 200.0);
        assert!((o.distance() - 4.0).abs() < 1e-9);
        assert!(o.eye[2] < 4.0 && o.eye[2] > 3.99, "stops short of the pole: {:?}", o.eye);
        assert!(o.pose().is_some());
    }

    #[test]
    fn looking_travelling_panning_and_dollying() {
        let mut o = Orbit { eye: [0.0, -4.0, 0.0], target: [0.0; 3], roll: 0.0, fov: 90.0 };
        let (right, up, forward) = o.axes().unwrap();
        assert!(
            close(right, [1.0, 0.0, 0.0]) && close(up, [0.0, 0.0, 1.0]) && close(forward, [0.0, 1.0, 0.0])
        );
        o.look(90.0, 0.0);
        // Turning right from looking along +y looks along +x.
        assert!(close(o.target, [4.0, -4.0, 0.0]), "{:?}", o.target);
        let mut o = Orbit { eye: [0.0, -4.0, 0.0], target: [0.0; 3], roll: 0.0, fov: 90.0 };
        o.travel(1.0, 2.0, 3.0);
        assert!(close(o.eye, [1.0, -1.0, 2.0]) && close(o.target, [1.0, 3.0, 2.0]));
        let mut o = Orbit { eye: [0.0, -4.0, 0.0], target: [0.0; 3], roll: 0.0, fov: 90.0 };
        // 90 degrees at distance 4 shows 8 from top to bottom.
        o.pan(0.5, 0.0);
        assert!(close(o.eye, [4.0, -4.0, 0.0]));
        o.dolly(0.25);
        assert!((o.distance() - 1.0).abs() < 1e-9 && close(o.target, [4.0, 0.0, 0.0]));
        o.dolly(f64::NAN);
        assert!((o.distance() - 1.0).abs() < 1e-9);
        // Rolled, right turns towards down.
        let rolled = Orbit { roll: 90.0, ..o };
        let (right, _, _) = rolled.axes().unwrap();
        assert!(close(right, [0.0, 0.0, -1.0]), "{right:?}");
    }
}
