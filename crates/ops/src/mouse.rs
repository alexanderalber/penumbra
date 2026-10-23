//! The mouse as channels: `chan.mouse_in` and the [`Mouse`] hub it reads.
//!
//! - **Position and buttons** are read from the whole desktop, inside penumbra or not
//!   ([`penumbra_system::pointer`]): polled when the node cooks, so the node is time dependent
//!   and costs nothing unless something shown or output reads it. A click shorter than a frame
//!   can be missed.
//! - **The wheel** is not polled but counted: the app's event loop listens to the mouse's raw
//!   input everywhere while a `chan.mouse_in` exists ([`Mouse::wanted`]) and passes each turn
//!   on. Raw input is a copy of what the mouse sends, not a hook in its path, so it cannot slow
//!   the desktop down. Headless runs have no event loop; there only `mouse.fake` turns it.
//! - **`display`** names the display the position is relative to, as an output's `display`
//!   does; empty means the display the pointer is on. `x` and `y` are shares of that display,
//!   origin bottom left (`y` 1 at the top, as a chart's axes); on a named display they follow the pointer beyond 0..1 when it leaves,
//!   and `inside` says whether it is on it.
//! - `mouse.fake` stands in for the pointer (tests, remote control); `mouse.status` reports.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use penumbra_system::Resolved;
use penumbra_system::pointer::{LEFT, MIDDLE, Monitor, Pointer, RIGHT};
use serde::Serialize;

/// Kind of the mouse input node.
pub const MOUSE_IN: &str = "chan.mouse_in";
/// Its output channels, in order.
pub const CHANNELS: [&str; 7] = ["x", "y", "inside", "left", "right", "middle", "wheel"];

/// How often the node looks at the displays again (one was plugged in or moved).
const MONITORS_EVERY: Duration = Duration::from_millis(500);

#[derive(Default)]
struct State {
    /// Wheel notches turned over output windows since the project opened (up positive).
    wheel: f64,
    /// Stands in for the real pointer while set.
    fake: Option<Pointer>,
}

/// What `mouse.status` reports.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// A `chan.mouse_in` is in the network, so the event loop passes the wheel on.
    pub wanted: bool,
    /// The pointer as the nodes see it now (`None`: cannot be read here).
    pub pointer: Option<Pointer>,
    pub faked: bool,
    pub wheel: f64,
    pub monitors: Vec<Monitor>,
}

/// The mouse hub: the wheel the event loop passes on, and a fake pointer. Cheap to clone.
#[derive(Clone, Default)]
pub struct Mouse {
    wanted: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
}

impl Mouse {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any `chan.mouse_in` exists; the event loop passes the wheel on only then.
    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    pub fn set_wanted(&self, on: bool) {
        self.wanted.store(on, Ordering::Relaxed);
    }

    /// Notches turned (up positive); non-finite amounts are dropped.
    pub fn wheel(&self, notches: f64) {
        if notches.is_finite() {
            let mut s = lock(&self.state);
            s.wheel = (s.wheel + notches).clamp(-1e12, 1e12);
        }
    }

    pub fn wheel_total(&self) -> f64 {
        lock(&self.state).wheel
    }

    /// Puts `pointer` in place of the real one (`None`: the real one again).
    pub fn fake(&self, pointer: Option<Pointer>) {
        lock(&self.state).fake = pointer;
    }

    /// The pointer now: the fake one while set, else the real one.
    pub fn pointer(&self) -> Option<Pointer> {
        let fake = lock(&self.state).fake;
        fake.or_else(penumbra_system::pointer::pointer)
    }

    /// Forgets the wheel (a project closes; the next starts at 0). A fake pointer stays.
    pub fn reset(&self) {
        lock(&self.state).wheel = 0.0;
    }

    pub fn status(&self) -> Status {
        let (faked, wheel) = {
            let s = lock(&self.state);
            (s.fake.is_some(), s.wheel)
        };
        Status {
            wanted: self.wanted(),
            pointer: self.pointer(),
            faked,
            wheel,
            monitors: penumbra_system::pointer::monitors(),
        }
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            MOUSE_IN,
            "The mouse pointer and its buttons as channels, anywhere on the desktop: steer or trigger things by hand during a show",
        )
        .param(ParamSpec::text("display", "").doc(
            "The display `x` and `y` are shares of, named as an output's `display` (or `\\\\.\\DISPLAY2`); \
             empty: the display the pointer is on",
        ))
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`x`, `y`: the pointer as shares of the display, 0 to 1 from the bottom left (`y` 1 at the top); \
             `inside`: 1 while it is on that display; `left`, `right`, `middle`: 1 while held; `wheel`: notches \
             turned anywhere, summed (up positive)",
        )
        .time_dependent(),
        || Box::new(MouseIn::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// Whether the network has a `chan.mouse_in`.
pub fn wanted(net: &Network) -> bool {
    net.nodes().any(|n| n.kind == MOUSE_IN)
}

/// The channel values for `pointer` against `monitor` (the display meant, `None` when it is
/// not connected), with `held` the position to keep when there is none.
pub fn values(pointer: Option<Pointer>, monitor: Option<&Monitor>, held: (f64, f64), wheel: f64) -> [f64; 7] {
    let gate = |p: &Pointer, bit: u8| if p.buttons & bit != 0 { 1.0 } else { 0.0 };
    let (x, y, inside) = match (pointer, monitor) {
        (Some(p), Some(m)) => {
            let (x, down) = m.share(p.x, p.y);
            (x, 1.0 - down, if m.contains(p.x, p.y) { 1.0 } else { 0.0 })
        }
        _ => (held.0, held.1, 0.0),
    };
    let (l, r, mid) =
        pointer.map_or((0.0, 0.0, 0.0), |p| (gate(&p, LEFT), gate(&p, RIGHT), gate(&p, MIDDLE)));
    [x, y, inside, l, r, mid, wheel]
}

#[derive(Default)]
struct MouseIn {
    monitors: Vec<Monitor>,
    monitors_at: Option<Instant>,
    /// `display` as last resolved: the text, and the OS name it means (or why none).
    resolved: Option<(String, Result<String, String>)>,
    out: Option<[f64; 7]>,
}

impl MouseIn {
    /// The display `text` names, resolved again when the text or the displays changed.
    fn target(&mut self, text: &str, displays_changed: bool) -> Result<String, String> {
        let stale = self.resolved.as_ref().is_none_or(|(t, _)| t != text);
        if stale || displays_changed {
            self.resolved = Some((text.to_string(), resolve(text)));
        }
        self.resolved.as_ref().map_or_else(|| Err(String::new()), |(_, r)| r.clone())
    }
}

/// The OS name of the display `text` names.
fn resolve(text: &str) -> Result<String, String> {
    let list = penumbra_system::displays().map_err(|e| format!("cannot list the displays: {}", e.0))?;
    match penumbra_system::resolve(text, &list) {
        Resolved::Exact(d) | Resolved::ByName(d) | Resolved::OldId(d) => Ok(d.os_name.clone()),
        Resolved::NotConnected { .. } => Err(format!("display `{text}` is not connected")),
        Resolved::Ambiguous(_) => Err(format!("`{text}` fits several displays; use the full id")),
    }
}

impl Operator for MouseIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Mouse>()
            .ok_or_else(|| CookError::failed("the mouse is not available in this process"))?;
        let now = Instant::now();
        let mut displays_changed = false;
        if self.monitors_at.is_none_or(|t| now.saturating_duration_since(t) >= MONITORS_EVERY) {
            let monitors = penumbra_system::pointer::monitors();
            displays_changed = monitors != self.monitors;
            self.monitors = monitors;
            self.monitors_at = Some(now);
        }
        let pointer = hub.pointer();
        let text = cx.text("display")?.trim().to_string();
        let (monitor, warning) = if text.is_empty() {
            (pointer.and_then(|p| self.monitors.iter().find(|m| m.contains(p.x, p.y)).cloned()), None)
        } else {
            match self.target(&text, displays_changed) {
                Ok(name) => match self.monitors.iter().find(|m| m.os_name == name) {
                    Some(m) => (Some(m.clone()), None),
                    None => (None, Some(format!("display `{text}` ({name}) is not on the desktop"))),
                },
                Err(e) => (None, Some(e)),
            }
        };
        if pointer.is_none() {
            cx.warn("the pointer cannot be read here");
        } else if let Some(w) = warning {
            cx.warn(w);
        }
        let held = self.out.map_or((0.0, 0.0), |o| (o[0], o[1]));
        let v = values(pointer, monitor.as_ref(), held, hub.wheel_total());
        if self.out == Some(v) && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.out = Some(v);
        let mut channels = Vec::with_capacity(CHANNELS.len());
        for (name, value) in CHANNELS.iter().zip(v) {
            let name = Name::new(name).map_err(|e| CookError::failed(e.0))?;
            channels.push(Channel { name, samples: vec![value] });
        }
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> Monitor {
        Monitor { os_name: r"\\.\DISPLAY2".into(), left: 1920, top: 0, right: 3840, bottom: 1080 }
    }

    #[test]
    fn shares_of_the_display_and_held_buttons() {
        let p = Pointer { x: 2880, y: 270, buttons: LEFT | MIDDLE };
        assert_eq!(values(Some(p), Some(&monitor()), (0.0, 0.0), 2.0), [0.5, 0.75, 1.0, 1.0, 0.0, 1.0, 2.0]);
        // Off the display: the shares go on, `inside` is 0.
        let off = Pointer { x: 960, y: 540, buttons: 0 };
        let v = values(Some(off), Some(&monitor()), (0.0, 0.0), 0.0);
        assert_eq!((v[0], v[1], v[2]), (-0.5, 0.5, 0.0));
        // No display or no pointer: the position holds, nothing is held.
        assert_eq!(values(Some(off), None, (0.3, 0.7), 0.0)[..3], [0.3, 0.7, 0.0]);
        assert_eq!(values(None, Some(&monitor()), (0.3, 0.7), 1.0), [0.3, 0.7, 0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn the_wheel_sums_and_a_fake_pointer_stands_in() {
        let m = Mouse::new();
        m.wheel(1.0);
        m.wheel(-3.0);
        m.wheel(f64::NAN);
        assert_eq!(m.wheel_total(), -2.0);
        let p = Pointer { x: 5, y: 6, buttons: RIGHT };
        m.fake(Some(p));
        assert_eq!(m.pointer(), Some(p));
        assert!(m.status().faked);
        m.reset();
        assert_eq!(m.wheel_total(), 0.0);
        assert_eq!(m.pointer(), Some(p), "a fake outlives a project");
        m.fake(None);
        assert!(!m.status().faked);
    }

    #[test]
    fn every_channel_name_is_valid() {
        for c in CHANNELS {
            assert!(Name::new(c).is_ok(), "{c}");
        }
    }
}
