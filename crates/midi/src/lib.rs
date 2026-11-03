//! MIDI in and out.
//!
//! - [`message`]: a bounded reader of the bytes a device sends, and the bytes of a message to
//!   send.
//! - [`store`]: the latest value per control of one device, and the names controls go by
//!   (`cc_1_7` as a channel, `/midi/<device>/cc/1/7` as an OSC address for learn).
//! - [`clock`]: MIDI clock as a tempo and a position in beats.
//! - [`hub`]: devices by name, opened and closed by one thread as they are wanted, plugged in
//!   and out; learn through the OSC hub; sending.
//!
//! No GPU, no UI, no graph: the node types live in `penumbra-ops`, the commands in
//! `penumbra-engine`. The drivers are reached through `midir` (WinMM on Windows).

pub mod clock;
pub mod hub;
pub mod message;
pub mod store;

pub use clock::{Clock, ClockState};
pub use hub::{
    Config, Hub, Ports, Snapshot, Status, Want, Watch, address, device_key, matches, parse_address,
};
pub use message::{Event, Reader, encode};
pub use store::{Control, Entry, Store};
