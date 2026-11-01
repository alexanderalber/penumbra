//! The networks, built from [`crate::layers`] and [`crate::ops`].

pub mod beat_this;
pub mod roformer;

pub use beat_this::{BeatLogits, BeatThis, BeatThisConfig};
