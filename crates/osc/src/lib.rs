//! OSC over UDP.
//!
//! - [`codec`]: a bounded parser and an encoder for OSC 1.0 messages and bundles. Own code
//!   rather than a crate, so every length is checked before anything is taken and the counts
//!   have caps of our choosing (the plan says why).
//! - [`pattern`]: address patterns (`*`, `?`, `[]`, `{}`), matched with a step budget.
//! - [`hub`]: listeners (one thread per port, localhost unless asked for the LAN, rate limits per
//!   source), what they received, the queue for param edits, learn, and sending.
//!
//! No GPU, no UI, no graph: the node types live in `penumbra-ops`, the param edits and the
//! commands in `penumbra-engine`.

pub mod codec;
pub mod hub;
pub mod pattern;

pub use codec::{Arg, Limits, Message, Packet};
pub use hub::{Config, Entry, Heard, Hub, Incoming, Sender, Status, Store, Want, Watch};
pub use pattern::{Budget, Pattern};

/// The channel name for an address: the parts joined by `_` (`/1/fader1` becomes `_1_fader1`,
/// a leading `_` where the name would start with a digit), anything but letters, digits and `_`
/// replaced by `_`, cut at 64 characters. `None` for an address with nothing left.
pub fn channel_name(address: &str) -> Option<String> {
    let mut s: String = address
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        return None;
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s.truncate(64);
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names() {
        assert_eq!(channel_name("/scene/speed").as_deref(), Some("scene_speed"));
        assert_eq!(channel_name("/1/fader1").as_deref(), Some("_1_fader1"));
        assert_eq!(channel_name("/a b.c").as_deref(), Some("a_b_c"));
        assert_eq!(channel_name("/"), None);
        assert_eq!(channel_name(&format!("/{}", "x".repeat(100))).map(|s| s.len()), Some(64));
    }
}
