//! Calibration math: pure functions, tested against synthetic data.
//!
//! - The planar part: a [`Homography`] from four or more point pairs by the normalised direct
//!   linear transform (Hartley and Zisserman, algorithm 4.2), for flat surfaces.
//! - The projector as a camera ([`Camera`]: [`Intrinsics`] and [`Pose`]) and its calibration
//!   from world and picture point pairs ([`calibrate`]): the normalised DLT of the 3x4
//!   projection (or a plane's homography) as the starting point, then Levenberg-Marquardt over
//!   pose and intrinsics.
//! - Structured light ([`scan`]): Gray-code patterns for a projector and the decoding of a
//!   camera's pictures of them into maps between camera and projector pixels; [`objects`]:
//!   the objects in front of a surface from two such scans, and the blend of projectors on them;
//!   [`relief`]: height maps of a mostly flat object from one scan, without a reference;
//!   [`scene`]: the planes of a scene and the objects on them from one scan ([`synthetic`]: a
//!   ray-cast room to test it on); [`epipolar`]: the geometry every right pair of camera and
//!   projector pixel obeys, which drops wrong decodes.
//! - Colour ([`colour`]): matching projectors to each other from a camera's pictures of their
//!   red, green and blue ramps; [`flatfield`]: brightness and colour that vary across one
//!   projector's picture, and the black level where projectors overlap.

mod camera;
pub mod colour;
pub mod epipolar;
pub mod flatfield;
mod homography;
pub mod objects;
mod raster;
pub mod relief;
pub mod scan;
pub mod scene;
mod solve;
pub mod synthetic;

pub use camera::{Camera, Intrinsics, MAX_COORDINATE, Pose, unproject_clip};
pub use homography::{Homography, MAX_PAIRS, ROBUST_SAMPLE, RobustFit};
pub use solve::{MAX_ITERATIONS, Options, Solution, Spread, Start, calibrate};

#[cfg(test)]
mod flatfield_tests;
#[cfg(test)]
mod objects_tests;
#[cfg(test)]
mod relief_tests;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod tests;
