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
//!   the objects in front of a surface from two such scans, and the blend of projectors on them.
//! - Colour ([`colour`]): matching projectors to each other from a camera's pictures of their
//!   red, green and blue ramps.

mod camera;
pub mod colour;
mod homography;
pub mod objects;
mod raster;
pub mod scan;
mod solve;

pub use camera::{Camera, Intrinsics, MAX_COORDINATE, Pose, unproject_clip};
pub use homography::{Homography, MAX_PAIRS};
pub use solve::{MAX_ITERATIONS, Options, Solution, Spread, Start, calibrate};

#[cfg(test)]
mod objects_tests;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod tests;
