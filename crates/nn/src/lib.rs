//! Neural network inference on the CPU, in plain Rust with no runtime library.
//!
//! - [`Tensor`]: a shape and its f32 values, row-major.
//! - [`ops`]: the operations on tensors (matrix products, convolution, norms, activations,
//!   rotary embeddings, attention, transposes). Every op checks its shapes and returns an
//!   [`Error`] instead of panicking.
//! - [`layers`]: weights with their op (a linear layer, a convolution, an attention block) as
//!   the models assemble them.
//! - [`safetensors`]: a reader for `.safetensors` files that treats the file as hostile.
//! - [`models`]: the networks. The first is Beat This! ([`models::beat_this`]).
//!
//! The models only call ops and layers, so a later backend (the GPU) can provide the same ops
//! without touching the model code. The large ops spread their work over rayon's thread pool.
//! A caller that runs many inputs at once can call the model from several rayon tasks.

pub mod layers;
pub mod models;
pub mod ops;
pub mod safetensors;
mod tensor;

pub use safetensors::SafeTensors;
pub use tensor::Tensor;

use std::fmt;

/// What went wrong in a load or an op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The shapes handed to an op do not fit together.
    Shape(String),
    /// A weights file is malformed, truncated or uses a format this crate does not read.
    Format(String),
    /// A weight the model needs is missing or has the wrong shape.
    Weights(String),
    /// Reading a file failed.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Shape(s) => write!(f, "shape mismatch: {s}"),
            Error::Format(s) => write!(f, "bad weights file: {s}"),
            Error::Weights(s) => write!(f, "weights do not fit the model: {s}"),
            Error::Io(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A shape error with a formatted message.
pub(crate) fn shape_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Shape(msg.into()))
}
