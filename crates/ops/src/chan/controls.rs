//! Controls on the node body: `chan.button`, `chan.slider` and `chan.dropdown`.
//!
//! - **`chan.slider`** makes one channel, its param `value` limited to `min`..`max`. Dragging on
//!   the node's body in the editor sets `value`; `node.set` does the same from outside. The
//!   value is a param, so it is saved with the project and one drag is one undo step.
//! - **`chan.button`** makes one channel that is 1 or 0. With `mode` `toggle` it is the param
//!   `value`: a click flips it (an edit, saved and undoable). With `mode` `momentary` it is 1
//!   while the button is held, in the editor, by `chan.press` or on a control page
//!   (`chan.http_in`); that is live state in the
//!   [`Buttons`] hub, never an edit, so it works while Claude edits and leaves no history. A
//!   press shorter than a frame still shows for one cook.
//! - **`chan.dropdown`** holds a list of `entries`, one per line, and `value`, the name of the
//!   entry picked. Its channel is the picked entry's place, counted from 0; its table output is
//!   one cell, the picked entry's name. A text or choice param bound to it reads that name
//!   ([`NodeDesc::binds_by_name`]), so several nodes follow one dropdown even when their own
//!   lists are in another order. The value is a param, like the slider's.
//! - **`show`** (all three): the control also appears on the show screen while show mode locks
//!   the project. There a toggle's, a slider's or a dropdown's `value` still changes, without a
//!   history entry and unsaved until show mode ends.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, Table, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, Waker,
};
use serde::Serialize;

/// Kind of the button node.
pub const BUTTON: &str = "chan.button";
/// Kind of the slider node.
pub const SLIDER: &str = "chan.slider";
/// Kind of the dropdown node.
pub const DROPDOWN: &str = "chan.dropdown";
/// Most entries a dropdown takes; the rest are left out with a warning.
pub const MAX_ENTRIES: usize = 256;
/// Longest entry a dropdown takes, in bytes.
pub const MAX_ENTRY: usize = 256;
/// The param that puts a control on the show screen.
pub const SHOW: &str = "show";
/// The button's `mode` choices.
pub const MOMENTARY: &str = "momentary";
pub const TOGGLE: &str = "toggle";

/// Who holds a momentary button down. Each holds on its own: the button is down while any does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Holder {
    /// The mouse on the node's body in the editor.
    Editor,
    /// `chan.press`.
    Command,
    /// One connection of a control page (`chan.http_in`), by its id: each page holds on its
    /// own, so one page letting go does not release another's press.
    Page(u64),
}

impl Holder {
    fn bit(self) -> u8 {
        match self {
            Holder::Editor => 1,
            Holder::Command => 2,
            Holder::Page(_) => 0,
        }
    }
}

/// Pages one button remembers as holding it; more are let go of the oldest first.
const MAX_PAGES: usize = 64;

/// One momentary button as the hub knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ButtonView {
    /// Held down by someone now.
    pub held: bool,
    /// Presses so far (held by two holders at once is one press).
    pub presses: u64,
}

#[derive(Default)]
struct Entry {
    holders: u8,
    /// The pages holding it (ids of [`Holder::Page`]).
    pages: Vec<u64>,
    presses: u64,
    waker: Option<Waker>,
}

/// The momentary buttons that are held, by node, and how often each was pressed. Cheap to
/// clone (a handle). The engine keeps one for the whole run and empties it when a project
/// closes.
#[derive(Clone, Default)]
pub struct Buttons(Arc<Mutex<BTreeMap<NodeId, Entry>>>);

impl Buttons {
    pub fn new() -> Self {
        Self::default()
    }

    /// `holder` presses the button of `node` (`down`) or lets go. Returns whether the button
    /// went down or up by it.
    pub fn hold(&self, node: NodeId, holder: Holder, down: bool) -> bool {
        let waker = {
            let mut map = lock(&self.0);
            let e = map.entry(node).or_default();
            let was = e.is_held();
            match (holder, down) {
                (Holder::Page(id), true) => {
                    if !e.pages.contains(&id) {
                        if e.pages.len() >= MAX_PAGES {
                            e.pages.remove(0);
                        }
                        e.pages.push(id);
                    }
                }
                (Holder::Page(id), false) => e.pages.retain(|p| *p != id),
                (_, true) => e.holders |= holder.bit(),
                (_, false) => e.holders &= !holder.bit(),
            }
            let is = e.is_held();
            if was == is {
                return false;
            }
            if is {
                e.presses += 1;
            }
            e.waker.clone()
        };
        if let Some(w) = waker {
            w.wake();
        }
        true
    }

    /// A press and a release at once: the button shows 1 for one cook.
    pub fn tap(&self, node: NodeId, holder: Holder) {
        self.hold(node, holder, true);
        self.hold(node, holder, false);
    }

    pub fn view(&self, node: NodeId) -> ButtonView {
        lock(&self.0)
            .get(&node)
            .map(|e| ButtonView { held: e.is_held(), presses: e.presses })
            .unwrap_or_default()
    }

    /// Forgets the nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Lets go of every button and forgets them (a project closes).
    pub fn reset(&self) {
        lock(&self.0).clear();
    }

    /// The node wakes when its button goes down or up.
    fn check_in(&self, node: NodeId, waker: Waker) -> ButtonView {
        let mut map = lock(&self.0);
        let e = map.entry(node).or_default();
        if e.waker.is_none() {
            e.waker = Some(waker);
        }
        ButtonView { held: e.is_held(), presses: e.presses }
    }
}

impl Entry {
    fn is_held(&self) -> bool {
        self.holders != 0 || !self.pages.is_empty()
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            BUTTON,
            "A button on the node itself, pressed with the mouse in the editor: 1 while held, or on and off with each click",
        )
        .param(
            ParamSpec::choice("mode", &[MOMENTARY, TOGGLE])
                .doc("What a click on the button does")
                .choice_doc(MOMENTARY, "1 while the mouse button is held on it, 0 again on release")
                .choice_doc(TOGGLE, "each click switches between 1 and 0, kept in `value` and saved with the project"),
        )
        .param(
            ParamSpec::bool("value", false)
                .doc("On or off in mode toggle. A click on the button flips it")
                .applies_when("mode", &[TOGGLE]),
        )
        .param(
            ParamSpec::text("label", "")
                .doc("Text on the button. Empty: \"press\", or \"on\" and \"off\". On the show screen it also names the button; empty: the node's name"),
        )
        .param(show_param())
        .param(super::name_param())
        .output("out", DataKind::Chan)
        .describe_output("out", "One channel: 1 while pressed or on, else 0"),
        || Box::new(Button::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            SLIDER,
            "A slider on the node itself, dragged with the mouse in the editor: a number between min and max",
        )
        .param(
            ParamSpec::float("value", 0.0)
                .range(0.0, 1.0)
                .doc("Where the slider stands, between min and max. Dragging on the node's body sets it"),
        )
        .param(ParamSpec::float("min", 0.0).range(-10.0, 10.0).doc("The value at the slider's left end"))
        .param(ParamSpec::float("max", 1.0).range(-10.0, 10.0).doc("The value at the slider's right end"))
        .param(
            ParamSpec::text("label", "").doc("The slider's name on the show screen. Empty: the node's name"),
        )
        .param(show_param())
        .param(super::name_param())
        .output("out", DataKind::Chan)
        .describe_output("out", "One channel: `value`, kept between min and max"),
        || Box::new(Slider::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            DROPDOWN,
            "A dropdown on the node itself: one entry picked from a list. A choice bound to it takes the entry of the same name",
        )
        .param(
            ParamSpec::text("entries", "a\nb\nc")
                .multiline()
                .lines(4)
                .doc("The entries to pick from, one per line. Spaces around an entry and empty lines are left out"),
        )
        .param(
            ParamSpec::text("value", "")
                .doc("The name of the entry picked. Empty: the first entry. Picking in the dropdown on the node sets it"),
        )
        .param(
            ParamSpec::text("label", "").doc("The dropdown's name on the show screen. Empty: the node's name"),
        )
        .param(show_param())
        .param(super::name_param())
        .binds_by_name()
        .output("out", DataKind::Chan)
        .describe_output("out", "One channel: the place of the entry picked, 0 for the first")
        .output("entry", DataKind::Table)
        .describe_output(
            "entry",
            "One cell: the name of the entry picked. A text or choice param bound to the node reads it",
        ),
        || Box::new(Dropdown::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// A dropdown's entries from its `entries` text: one per line, trimmed, without empty lines and
/// without repeats (case aside), at most [`MAX_ENTRIES`] of at most [`MAX_ENTRY`] bytes. The
/// second part says what was left out, for a warning.
pub fn dropdown_entries(text: &str) -> (Vec<String>, Vec<String>) {
    let mut entries: Vec<String> = Vec::new();
    let mut dropped = Vec::new();
    for line in text.lines() {
        let entry = line.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.len() > MAX_ENTRY || entry.chars().any(char::is_control) {
            dropped.push(format!("an entry longer than {MAX_ENTRY} bytes or with control characters"));
        } else if entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
            dropped.push(format!("\"{entry}\" a second time"));
        } else if entries.len() >= MAX_ENTRIES {
            dropped.push(format!("the entries after the first {MAX_ENTRIES}"));
            break;
        } else {
            entries.push(entry.to_string());
        }
    }
    (entries, dropped)
}

/// The place of the entry `value` names among `entries`, case and surrounding spaces aside; an
/// empty `value` picks the first. `None`: no entry has that name, or there are none.
pub fn dropdown_pick(entries: &[String], value: &str) -> Option<usize> {
    let value = value.trim();
    if entries.is_empty() {
        return None;
    }
    if value.is_empty() {
        return Some(0);
    }
    entries.iter().position(|e| e.eq_ignore_ascii_case(value))
}

fn show_param() -> ParamSpec {
    ParamSpec::bool(SHOW, false).doc(
        "The control also appears on the show screen while show mode is on. There it steers the show \
         without the editor. A change there makes no history entry and is saved when show mode ends",
    )
}

/// `value` kept between `min` and `max` (either order); a non-finite value is `min`.
pub fn slider_value(value: f64, min: f64, max: f64) -> f64 {
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    if !value.is_finite() || !lo.is_finite() || !hi.is_finite() {
        return if lo.is_finite() { lo } else { 0.0 };
    }
    value.clamp(lo, hi)
}

fn one_channel(cx: &CookContext<'_>, value: f64) -> Result<Cooked, CookError> {
    let name = Name::new(cx.text("name")?.trim()).map_err(|e| e.context("`name`"))?;
    let chans = Chans::new(cx.rate().as_f64(), vec![Channel { name, samples: vec![value] }])?;
    Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
}

#[derive(Default)]
struct Button {
    /// Presses seen at the last cook, so a tap between two cooks still shows.
    seen: Option<u64>,
    out: Option<f64>,
}

impl Operator for Button {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let value = if cx.text("mode")? == TOGGLE {
            self.seen = None;
            if cx.bool("value")? { 1.0 } else { 0.0 }
        } else {
            // Without the hub (a cooker outside the app) nobody can press it: up.
            let view =
                cx.resource::<Buttons>().map(|hub| hub.check_in(cx.node(), cx.waker())).unwrap_or_default();
            let tapped = self.seen.is_some_and(|s| s != view.presses) && !view.held;
            self.seen = Some(view.presses);
            if tapped {
                // Back to 0 at the next frame.
                cx.waker().wake();
            }
            if view.held || tapped { 1.0 } else { 0.0 }
        };
        if self.out == Some(value) && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.out = Some(value);
        one_channel(cx, value)
    }
}

#[derive(Default)]
struct Slider {
    out: Option<f64>,
}

impl Operator for Slider {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let value = slider_value(cx.float("value")?, cx.float("min")?, cx.float("max")?);
        if self.out == Some(value) && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.out = Some(value);
        one_channel(cx, value)
    }
}

#[derive(Default)]
struct Dropdown {
    /// The place and name handed out last.
    out: Option<(usize, String)>,
}

impl Operator for Dropdown {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let (entries, dropped) = dropdown_entries(cx.text("entries")?);
        if !dropped.is_empty() {
            cx.warn(format!("left out of `entries`: {}", dropped.join(", ")));
        }
        if entries.is_empty() {
            return Err(CookError::failed(
                "`entries` is empty: write the entries to pick from, one per line",
            ));
        }
        let value = cx.text("value")?;
        let place = match dropdown_pick(&entries, value) {
            Some(i) => i,
            None => {
                // The entry handed out last stays while it is still listed, so bound nodes do not
                // jump while the list is edited. Otherwise the first entry goes out.
                let kept = self.out.as_ref().and_then(|(_, name)| dropdown_pick(&entries, name)).unwrap_or(0);
                cx.warn(format!(
                    "`value` \"{}\" is none of the entries ({}); the dropdown gives \"{}\" until `value` names one",
                    value.trim().escape_debug(),
                    entries.join(", "),
                    entries.get(kept).map_or("", String::as_str)
                ));
                kept
            }
        };
        let name = entries.get(place).cloned().unwrap_or_default();
        if self.out.as_ref().is_some_and(|(p, n)| *p == place && *n == name)
            && !cx.params_changed()
            && !cx.first_cook()
        {
            return Ok(Cooked::Unchanged);
        }
        let channel = Name::new(cx.text("name")?.trim()).map_err(|e| e.context("`name`"))?;
        let chans =
            Chans::new(cx.rate().as_f64(), vec![Channel { name: channel, samples: vec![place as f64] }])?;
        let table = Table::new(vec![vec![name.clone()]])?;
        self.out = Some((place, name));
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans), Data::new(DataKind::Table, table)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_entries_are_trimmed_lines_without_repeats() {
        let (entries, dropped) = dropdown_entries("  coral \n\nmaze\r\nMaze\nworms\n");
        assert_eq!(entries, ["coral", "maze", "worms"]);
        assert_eq!(dropped, ["\"Maze\" a second time"]);
        assert_eq!(dropdown_pick(&entries, ""), Some(0), "empty picks the first");
        assert_eq!(dropdown_pick(&entries, " WORMS "), Some(2), "case and spaces aside");
        assert_eq!(dropdown_pick(&entries, "holes"), None);
        assert_eq!(dropdown_pick(&[], ""), None);
        let many: String = (0..300).map(|i| format!("e{i}\n")).collect();
        let (entries, dropped) = dropdown_entries(&many);
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn slider_value_stays_in_range() {
        assert_eq!(slider_value(0.5, 0.0, 1.0), 0.5);
        assert_eq!(slider_value(2.0, 0.0, 1.0), 1.0);
        assert_eq!(slider_value(-3.0, 0.0, 1.0), 0.0);
        assert_eq!(slider_value(5.0, 10.0, 0.0), 5.0, "min above max");
        assert_eq!(slider_value(f64::NAN, -1.0, 1.0), -1.0);
        assert_eq!(slider_value(0.5, f64::NAN, f64::NAN), 0.0);
    }

    #[test]
    fn holders_hold_on_their_own_and_presses_count_once() {
        let b = Buttons::new();
        let n = NodeId(7);
        assert_eq!(b.view(n), ButtonView::default());
        assert!(b.hold(n, Holder::Editor, true));
        assert!(!b.hold(n, Holder::Command, true), "already down");
        assert!(!b.hold(n, Holder::Editor, false), "the command still holds it");
        assert_eq!(b.view(n), ButtonView { held: true, presses: 1 });
        assert!(b.hold(n, Holder::Command, false));
        assert!(!b.hold(n, Holder::Command, false), "a second release changes nothing");
        b.tap(n, Holder::Command);
        assert_eq!(b.view(n), ButtonView { held: false, presses: 2 });
        assert!(b.hold(n, Holder::Page(1), true));
        assert!(!b.hold(n, Holder::Page(2), true), "a second page holds too");
        assert!(!b.hold(n, Holder::Page(1), false), "the second page still holds it");
        assert!(b.hold(n, Holder::Page(2), false));
        assert_eq!(b.view(n), ButtonView { held: false, presses: 3 });
        b.hold(n, Holder::Editor, true);
        b.reset();
        assert_eq!(b.view(n), ButtonView::default(), "a closing project lets go");
        b.hold(n, Holder::Editor, true);
        b.prune(|id| id != n);
        assert_eq!(b.view(n), ButtonView::default());
    }
}
