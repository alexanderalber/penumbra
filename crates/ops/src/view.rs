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
pub const CAMERA_PARAMS: [&str; 8] = ["position", "target", "fov", "roll", "shift", "camera", "near", "far"];

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
        let json = cx.text("camera")?.trim();
        if !json.is_empty() {
            let c: Camera = serde_json::from_str(json)
                .map_err(|e| CookError::failed(format!("`camera` is not a camera: {e}")))?;
            if c.intrinsics.size.contains(&0) {
                return Err(CookError::failed("`camera` has no picture size"));
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

/// The camera's rarely used params: `shift`, `camera`, `near`, `far`.
pub fn more_params(desc: NodeDesc) -> NodeDesc {
    desc.param(
        ParamSpec::vec2("shift", [0.0; 2])
            .range(-1.0, 1.0)
            .rarely_used()
            .doc("Lens shift in picture sizes: [0, 0.5] moves the picture up by half its height"),
    )
    .param(ParamSpec::text("camera", "").rarely_used().doc(
        "A calibrated camera as JSON (calib.solve's `camera`); replaces position, target, fov, roll and shift",
    ))
    .param(
        ParamSpec::float("near", 0.05)
            .clamp(1e-4, 1e4)
            .curve(Curve::Log)
            .rarely_used()
            .doc("Nearest distance drawn"),
    )
    .param(
        ParamSpec::float("far", 100.0)
            .clamp(1e-3, 1e6)
            .curve(Curve::Log)
            .rarely_used()
            .doc("Farthest distance drawn"),
    )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = more_params(params(NodeDesc::new(
        CAMERA,
        "A camera: where it is, what it looks at, its lens; for tex.render's camera input",
    )))
    .output("out", DataKind::View);
    types.register(FnNodeType::new(desc, || Box::new(CameraNode) as Box<dyn Operator>))
}

struct CameraNode;

impl Operator for CameraNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        Ok(Cooked::Outputs(vec![Data::new(DataKind::View, Viewpoint::from_params(cx)?)]))
    }
}
