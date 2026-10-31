//! HTTP and WebSocket in, for control pages in a browser (a phone on the LAN) and for other
//! programs.
//!
//! - [`request`]: a bounded HTTP/1.1 request parser and the responses.
//! - [`ws`]: WebSocket frames and the handshake (SHA-1 and base64 included, a few lines each).
//! - [`message`]: what a message says (`{"/fader": 0.5}` or `?fader=0.5`) as OSC messages and
//!   tables.
//! - [`files`]: static files from one folder, with no way out of it.
//! - [`hub`]: listeners (a thread per port, one per connection, localhost unless asked for the
//!   LAN), caps on everything, the state sent back to WebSocket clients.
//!
//! Own code on `std::net` rather than a server crate: no async runtime, every cap of our
//! choosing, nothing new to audit. Messages are handed to the OSC hub, so params by address and
//! learn work as for OSC. The node lives in `penumbra-ops`, the commands in `penumbra-engine`.

pub mod files;
pub mod hub;
pub mod message;
pub mod request;
pub mod ws;

pub use hub::{Config, Hub, ListenerStatus, Params, Publisher, Tables, Want, Watch};
