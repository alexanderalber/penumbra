//! Calibration math: pure functions, tested against synthetic data.
//!
//! - The planar part: a [`Homography`] from four or more point pairs by the normalised direct
//!   linear transform (Hartley and Zisserman, algorithm 4.2), for flat surfaces.
//! - The projector as a camera ([`Camera`]: [`Intrinsics`] and [`Pose`]) and its calibration
//!   from world and picture point pairs ([`calibrate`]): the normalised DLT of the 3x4
//!   projection (or a plane's homography) as the starting point, then Levenberg-Marquardt over
//!   pose and intrinsics.

mod camera;
mod homography;
mod solve;

pub use camera::{Camera, Intrinsics, MAX_COORDINATE, Pose};
pub use homography::{Homography, MAX_PAIRS};
pub use solve::{MAX_ITERATIONS, Options, Solution, Spread, Start, calibrate};

#[cfg(test)]
mod tests;
