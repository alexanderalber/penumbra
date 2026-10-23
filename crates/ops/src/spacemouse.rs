//! The 3D mouse as channels: `chan.spacemouse_in`, reading the one [`SpaceNav`] reader the
//! viewer's navigation reads too. Reading takes nothing away: the node and a viewer flying a
//! free view both follow the cap at the same time (button presses are counted per reader,
//! [`SpaceNav::press_counts`]). Axes are shaped as for the viewer: resting offset taken out,
//! dead zone, response curve; each -1..1.
//!
//! The node is time dependent: it cooks every frame while something shown or output reads it,
//! and costs nothing otherwise. A `chan.spacemouse_in` in the network opens the device.

use penumbra_core::{Channel, Chans, Error, Name};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use penumbra_spacenav::{BUTTONS, SpaceNav, State};

/// Kind of the 3D mouse input node.
pub const SPACEMOUSE_IN: &str = "chan.spacemouse_in";
/// The axes as a flight ([`penumbra_spacenav::Motion::fly`]), in channel order.
pub const AXES: [&str; 6] = ["right", "up", "forward", "pitch", "yaw", "roll"];
/// The buttons' gates, bit 0 first.
pub const BUTTON_CHANNELS: [&str; BUTTONS] = ["button_left", "button_right"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            SPACEMOUSE_IN,
            "The 3D mouse (SpaceNavigator) as channels: six axes and two buttons, to steer things by hand during a show",
        )
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "-1 to 1 each: `right` (slide the cap right), `up` (lift it), `forward` (push it away), `pitch` (tilt \
             it, nose up), `yaw` (twist it clockwise), `roll` (tilt it sideways, to the right); `button_left`, \
             `button_right`: 1 while held; `connected`: 1 while the device is there",
        )
        .time_dependent(),
        || Box::new(SpaceMouseIn::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// Whether the network has a `chan.spacemouse_in`.
pub fn wanted(net: &Network) -> bool {
    net.nodes().any(|n| n.kind == SPACEMOUSE_IN)
}

#[derive(Default)]
struct SpaceMouseIn {
    /// Presses per button at the last cook, so a press shorter than a frame still shows.
    seen: Option<[u64; BUTTONS]>,
    out: Vec<f64>,
}

/// The channel values: axes, button gates, `connected`.
fn values(nav: &SpaceNav, seen: &mut Option<[u64; BUTTONS]>) -> (Vec<f64>, bool) {
    let motion = nav.motion();
    let fly = motion.fly();
    let counts = nav.press_counts();
    // A first cook starts from what was counted before: earlier presses are no tap.
    let before = seen.unwrap_or(counts);
    *seen = Some(counts);
    let mut v: Vec<f64> = [fly.right, fly.up, fly.forward, fly.pitch, fly.yaw, fly.roll]
        .iter()
        .map(|a| f64::from(*a))
        .collect();
    let mut tapped = false;
    for (i, (now, was)) in counts.iter().zip(before).enumerate() {
        let held = motion.buttons & (1 << i) != 0;
        let tap = *now != was && !held;
        tapped |= tap;
        v.push(if held || tap { 1.0 } else { 0.0 });
    }
    v.push(if nav.status().state == State::Connected { 1.0 } else { 0.0 });
    (v, tapped)
}

impl Operator for SpaceMouseIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let nav = cx
            .resource::<SpaceNav>()
            .ok_or_else(|| CookError::failed("the 3D mouse is not available in this process"))?;
        nav.start();
        let status = nav.status();
        match status.state {
            State::Connected | State::Off => {}
            State::Searching => cx.warn("no 3D mouse plugged in (looking for it every second)"),
            State::Failed => cx.warn(format!(
                "the 3D mouse cannot be opened: {}",
                status.message.as_deref().unwrap_or("unknown reason")
            )),
            State::Unsupported => cx.warn("the 3D mouse is read on Windows only"),
        }
        let (v, tapped) = values(nav, &mut self.seen);
        if tapped {
            // Up again at the next frame.
            cx.waker().wake();
        }
        if v == self.out && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.out = v.clone();
        let names = AXES.iter().chain(&BUTTON_CHANNELS).chain(&["connected"]);
        let mut channels = Vec::with_capacity(v.len());
        for (name, value) in names.zip(v) {
            let name = Name::new(name).map_err(|e| CookError::failed(e.0))?;
            channels.push(Channel { name, samples: vec![value] });
        }
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

/// For learn: the channel of what the cap does most now (an axis beyond half way), or of a
/// button pressed since `counts` was taken.
pub fn heard(nav: &SpaceNav, counts: [u64; BUTTONS]) -> Option<&'static str> {
    let now = nav.press_counts();
    if let Some(i) = now.iter().zip(counts).position(|(n, c)| *n != c) {
        return BUTTON_CHANNELS.get(i).copied();
    }
    let fly = nav.motion().fly();
    let axes = [fly.right, fly.up, fly.forward, fly.pitch, fly.yaw, fly.roll];
    let (i, a) = axes.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    if a.abs() >= 0.5 { AXES.get(i).copied() } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_and_buttons_as_channels() {
        let nav = SpaceNav::new();
        let mut seen = None;
        let (v, tapped) = values(&nav, &mut seen);
        assert_eq!(v, vec![0.0; 9], "at rest, not connected");
        assert!(!tapped);
        nav.feed(&[1, 0xc2, 0x01, 0, 0, 0, 0]); // tx = 450: slid right
        nav.feed(&[3, 2, 0, 0, 0, 0, 0]); // right button down
        let (v, _) = values(&nav, &mut seen);
        assert_eq!(v[0], 1.0, "{v:?}");
        assert_eq!(&v[6..8], &[0.0, 1.0]);
        // A press and release between two cooks shows once.
        nav.feed(&[3, 3, 0, 0, 0, 0, 0]);
        nav.feed(&[3, 0, 0, 0, 0, 0, 0]);
        let (v, tapped) = values(&nav, &mut seen);
        assert_eq!(&v[6..8], &[1.0, 0.0]);
        assert!(tapped);
        let (v, _) = values(&nav, &mut seen);
        assert_eq!(&v[6..8], &[0.0, 0.0]);
        // Taken presses (the viewer's) leave the node's count alone.
        assert_eq!(nav.take_presses(), 3);
        assert_eq!(nav.press_counts(), [1, 1], "the right one was held through");
    }

    #[test]
    fn learn_hears_a_button_or_the_axis_moved_most() {
        let nav = SpaceNav::new();
        let counts = nav.press_counts();
        assert_eq!(heard(&nav, counts), None);
        nav.feed(&[2, 0, 0, 0, 0, 0x90, 0x01]); // rz = 400 of 450: twisted
        assert_eq!(heard(&nav, counts), Some("yaw"));
        nav.feed(&[3, 1, 0, 0, 0, 0, 0]);
        assert_eq!(heard(&nav, counts), Some("button_left"), "a button wins");
    }
}
