//! The vocabulary every other penumbra crate shares: show time in integer ticks, ids, names
//! and parameter paths, parameter values and their metadata, and the common error type.
//!
//! No GPU, no UI, no threads.

pub mod chan;
pub mod error;
pub mod id;
pub mod param;
pub mod path;
pub mod table;
pub mod time;
pub mod value;

pub use chan::{Channel, Chans};
pub use error::Error;
pub use id::{GroupId, NodeId};
pub use param::{Curve, ParamKind, ParamSpec, Range, Unit};
pub use path::{Name, NodePath, ParamPart, ParamPath};
pub use table::Table;
pub use time::{
    Advance, FrameTime, LatePolicy, PaceSource, Pacer, Rate, ShowClock, TICKS_PER_SECOND, Tick, TimerPacer,
};
pub use value::Value;
