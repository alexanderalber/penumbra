//! DMX512 out and in.
//!
//! - [`serial`]: the Enttec Open DMX USB, an FTDI FT232R with an RS-485 driver and no
//!   microcontroller, so the host makes the whole signal: 250 kbaud, 8N2, a break before every
//!   frame, sent again and again from a thread of its own.
//! - [`artnet`], [`sacn`], [`ddp`]: the packets of the three network protocols (Art-Net 4,
//!   E1.31, DDP for pixel controllers such as WLED).
//! - [`hub`]: one output per target (port or address), a thread each that sends the newest
//!   frame at a steady rate whether or not anything changed (fixtures and sACN receivers expect
//!   that), several writers per output merged by their slot ranges, status and timing.
//! - [`input`]: Art-Net and sACN received, a listener thread per protocol, interface and port
//!   that keeps the latest frame of the universes asked for, sources merged by priority and
//!   highest value.
//!
//! No GPU, no UI, no graph: the node type lives in `penumbra-ops`, the commands in
//! `penumbra-engine`.

pub mod artnet;
pub mod ddp;
pub mod hub;
pub mod input;
pub mod sacn;
pub mod serial;

pub use hub::{Config, Hub, Output, Stats, Status, Target};
pub use input::{Input, Inputs, Listen, Protocol};

/// Slots (channels) in one DMX universe.
pub const SLOTS: usize = 512;
