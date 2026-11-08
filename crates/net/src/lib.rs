//! Lines, JSON and bytes over serial ports, UDP and TCP.
//!
//! - [`frame`]: where a message ends in a byte stream: at a delimiter (`\n` by default), after a
//!   fixed number of bytes, or with each read. Messages over the cap are dropped and counted.
//! - [`parse`]: a message as named values ([`parse::Field`]): a line split into fields
//!   (`1,2,3`, `x:1 y=2`), a JSON object flattened (`{"acc": {"x": 1}}` is `acc_x`), or bytes;
//!   and the [`parse::Store`] of a reader: the latest number per name and the last messages as
//!   table rows.
//! - [`encode`]: channels and table rows as lines, JSON or bytes, for sending.
//! - [`hub`]: one link per port or peer, with a thread of its own (a thread per client for a
//!   TCP listener). Readers and writers on the same link share it, so a node reading a serial
//!   port and one writing to it use one open port. Links reconnect with backoff; nothing here
//!   blocks the caller.
//!
//! No GPU, no UI, no graph: the node types live in `penumbra-ops`, the commands in
//! `penumbra-engine`.

pub mod encode;
pub mod frame;
pub mod hub;
pub mod parse;

pub use hub::{Endpoint, Hub, Reader, Status, Writer};
pub use parse::{Decode, Format, Split, Store};
