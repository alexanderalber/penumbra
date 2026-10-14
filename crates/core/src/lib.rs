//! The vocabulary every other penumbra crate shares: show time in integer ticks, ids, names
//! and parameter paths, parameter values and their metadata, the common error type, and a lock
//! that survives a panic elsewhere.
//!
//! No GPU, no UI, no threads.

pub mod chan;
pub mod error;
pub mod id;
pub mod param;
pub mod path;
pub mod sync;
pub mod table;
pub mod time;
pub mod value;

pub use chan::{Channel, Chans};
pub use error::Error;
pub use id::{GroupId, NodeId};
pub use param::{Curve, MORE, ParamKind, ParamSpec, Range, Unit};
pub use path::{Name, NodePath, ParamPart, ParamPath};
pub use sync::lock;
pub use table::Table;
pub use time::{
    Advance, FrameTime, LatePolicy, PaceSource, Pacer, Rate, ShowClock, TICKS_PER_SECOND, Tick, TimerPacer,
};
pub use value::Value;
