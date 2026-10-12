//! No HID backend on this system yet.

use crate::{Shared, State};

pub(crate) fn run(shared: &Shared) {
    shared.set_state(State::Unsupported, None, Some("not supported on this system yet".into()));
}
