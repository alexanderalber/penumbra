//! Keyboard input: `chan.keyboard_in` and the [`Keyboard`] hub it reads.
//!
//! - **The hub** holds which keys are down and when each was pressed last. The windows feed it:
//!   the editor (and the viewer's own window) and the projector outputs, each as a [`Source`],
//!   and only while one of them has the focus; there is no global hook, so a key typed in
//!   another program never reaches the show. The editor leaves out keys typed into a text
//!   field. A command can press keys too (`keyboard.press`). The windows look at
//!   [`Keyboard::wanted`] first, so without a `chan.keyboard_in` in the network a key costs one
//!   atomic load.
//! - **Key names** ([`Key::parse`]): digits `0` to `9` (the number pad's digits are the same
//!   keys), letters `a` to `z` (as the layout labels them), `f1` to `f12`, `space`, the arrows
//!   `left right up down`, `enter tab backspace delete insert home end pageup pagedown escape`.
//! - **`chan.keyboard_in`** makes one gate per listed key (`key_1`: 1 while held; a press shorter
//!   than a frame still shows for one cook) and `last`, the 0-based index in `keys` of the
//!   listed key pressed last. `last` holds its value and starts at 0; it is 0 again when the
//!   project is opened anew. The node recooks when a key goes down or up, not every frame.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator, Waker,
};
use serde::Serialize;

/// Kind of the keyboard input node.
pub const KEYBOARD_IN: &str = "chan.keyboard_in";
/// What `keys` lists unless set: the number keys that switch four show pieces.
pub const DEFAULT_KEYS: &str = "1 2 3 4";
/// The prefix of a key's gate channel: `key_1`, `key_space`.
pub const GATE_PREFIX: &str = "key_";
/// Keys one node lists at most.
pub const MAX_KEYS: usize = 128;

/// Every key name, in a fixed order.
const NAMES: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "space",
    "left",
    "right",
    "up",
    "down",
    "enter",
    "tab",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "escape",
];

/// Other spellings of a key name.
const ALIASES: &[(&str, &str)] = &[
    ("arrowleft", "left"),
    ("arrowright", "right"),
    ("arrowup", "up"),
    ("arrowdown", "down"),
    ("return", "enter"),
    ("esc", "escape"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
    ("spacebar", "space"),
];

/// A key by its name ([`Key::parse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(&'static str);

impl Key {
    /// A key from its name or an alias, any case. The number pad's digits (`numpad1`, `num1`,
    /// `kp1`) are the digits; so are `digit1` and `key1`, and `keya` is `a` (the names winit and
    /// the browser give the physical keys).
    pub fn parse(text: &str) -> Option<Key> {
        let lower = text.trim().to_ascii_lowercase();
        if lower.is_empty() || lower.len() > 16 {
            return None;
        }
        let mut name = lower.as_str();
        if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == name) {
            name = to;
        }
        for prefix in ["numpad", "num", "kp", "digit", "key"] {
            if let Some(rest) = name.strip_prefix(prefix)
                && rest.len() == 1
                && rest.chars().all(|c| c.is_ascii_alphanumeric())
                && (prefix == "key" || rest.chars().all(|c| c.is_ascii_digit()))
            {
                name = rest;
            }
        }
        NAMES.iter().find(|n| **n == name).map(|n| Key(n))
    }

    /// The canonical name: `1`, `a`, `f5`, `space`, `left`.
    pub fn name(self) -> &'static str {
        self.0
    }

    /// The name of its gate channel: `key_1`.
    pub fn channel(self) -> String {
        format!("{GATE_PREFIX}{}", self.0)
    }

    /// Every key, for docs and tests.
    pub fn all() -> impl Iterator<Item = Key> {
        NAMES.iter().map(|n| Key(n))
    }
}

/// Where a key event comes from. Each source holds its keys on its own: a window that loses
/// the focus lets go of what it held ([`Keyboard::release_all`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The editor's main window.
    Editor,
    /// The viewer in its own window.
    Viewer,
    /// A projector output window.
    Output,
    /// `keyboard.press`.
    Command,
}

impl Source {
    fn bit(self) -> u8 {
        match self {
            Source::Editor => 1,
            Source::Viewer => 2,
            Source::Output => 4,
            Source::Command => 8,
        }
    }
}

/// One key as the hub knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyView {
    /// Down in some source.
    pub held: bool,
    /// Presses so far (a key held in two sources at once is one press).
    pub presses: u64,
    /// The press count of all keys at its last press; 0 before its first.
    pub last: u64,
}

#[derive(Default)]
struct KeyState {
    /// Sources holding it down, as [`Source::bit`]s.
    holders: u8,
    presses: u64,
    last: u64,
}

#[derive(Default)]
struct State {
    /// Presses of all keys so far.
    seq: u64,
    keys: BTreeMap<Key, KeyState>,
    /// What each source holds, by the physical key that went down (a release names the
    /// physical key; its meaning may have changed in between, e.g. with Shift).
    held: BTreeMap<(Source, String), Key>,
    watchers: Vec<(u64, Waker)>,
    next_watch: u64,
    learning: bool,
    heard: Option<Key>,
    last_key: Option<Key>,
}

impl State {
    fn holders_of(&self, key: Key) -> u8 {
        self.held.iter().filter(|(_, k)| **k == key).fold(0, |acc, ((s, _), _)| acc | s.bit())
    }

    /// Brings the holders of `key` in line with `held`; true when it went up.
    fn settle(&mut self, key: Key) -> bool {
        let holders = self.holders_of(key);
        let ks = self.keys.entry(key).or_default();
        let released = ks.holders != 0 && holders == 0;
        ks.holders = holders;
        released
    }

    fn wakers(&self) -> Vec<Waker> {
        self.watchers.iter().map(|(_, w)| w.clone()).collect()
    }
}

/// What `keyboard.status` reports.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// A `chan.keyboard_in` is in the network, so the windows pass keys on.
    pub wanted: bool,
    /// Keys down now, with who holds them.
    pub held: Vec<(String, Vec<Source>)>,
    /// The key pressed last, if any since the project opened.
    pub last: Option<String>,
    /// Presses since the project opened.
    pub presses: u64,
}

/// The keyboard hub: which keys are down, who pressed what last. Cheap to clone (a handle).
#[derive(Clone, Default)]
pub struct Keyboard {
    wanted: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
}

/// Wakes a node while it lives; dropping it stops that.
pub struct Watch {
    id: u64,
    state: std::sync::Weak<Mutex<State>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            lock(&state).watchers.retain(|(id, _)| *id != self.id);
        }
    }
}

impl Keyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any `chan.keyboard_in` exists; the windows pass keys on only then.
    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    /// Set when the network changes. Off lets go of every key.
    pub fn set_wanted(&self, on: bool) {
        if self.wanted.swap(on, Ordering::Relaxed) && !on {
            for s in [Source::Editor, Source::Viewer, Source::Output, Source::Command] {
                self.release_all(s);
            }
        }
    }

    /// A key went down (`down`) or up in `source`. `physical` names the key as the source
    /// knows it, whatever it means; `key` is what it means now (`None`: no key penumbra
    /// names, ignored when it goes down). A repeat of a key held is no press.
    pub fn key(&self, source: Source, physical: &str, key: Option<Key>, down: bool) {
        let wake = {
            let mut s = lock(&self.state);
            let slot = (source, physical.to_string());
            if down {
                let Some(key) = key else { return };
                if s.held.contains_key(&slot) {
                    return;
                }
                s.held.insert(slot, key);
                let was_up = s.keys.get(&key).is_none_or(|k| k.holders == 0);
                s.settle(key);
                if !was_up {
                    return;
                }
                s.seq += 1;
                let seq = s.seq;
                let ks = s.keys.entry(key).or_default();
                ks.presses += 1;
                ks.last = seq;
                s.last_key = Some(key);
                if s.learning {
                    s.heard = Some(key);
                }
            } else {
                let Some(key) = s.held.remove(&slot) else { return };
                if !s.settle(key) {
                    return;
                }
            }
            s.wakers()
        };
        for w in wake {
            w.wake();
        }
    }

    /// Presses and releases `key` (a tap), as a command does.
    pub fn tap(&self, key: Key) {
        self.key(Source::Command, key.name(), Some(key), true);
        self.key(Source::Command, key.name(), Some(key), false);
    }

    /// Lets go of everything `source` holds (its window lost the focus).
    pub fn release_all(&self, source: Source) {
        let wake = {
            let mut s = lock(&self.state);
            let keys: Vec<Key> =
                s.held.iter().filter(|((src, _), _)| *src == source).map(|(_, k)| *k).collect();
            if keys.is_empty() {
                return;
            }
            s.held.retain(|(src, _), _| *src != source);
            let mut released = false;
            for k in keys {
                released |= s.settle(k);
            }
            if !released {
                return;
            }
            s.wakers()
        };
        for w in wake {
            w.wake();
        }
    }

    /// Forgets every press (a project closes; the next starts with `last` at 0).
    pub fn reset(&self) {
        let wake = {
            let mut s = lock(&self.state);
            s.seq = 0;
            s.keys.clear();
            s.held.clear();
            s.heard = None;
            s.last_key = None;
            s.wakers()
        };
        for w in wake {
            w.wake();
        }
    }

    /// The state of `keys`, in their order.
    pub fn view(&self, keys: &[Key]) -> Vec<KeyView> {
        let s = lock(&self.state);
        keys.iter()
            .map(|k| {
                s.keys
                    .get(k)
                    .map(|ks| KeyView { held: ks.holders != 0, presses: ks.presses, last: ks.last })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Calls `waker` whenever a key goes down or up, until the watch is dropped.
    pub fn watch(&self, waker: Waker) -> Watch {
        let mut s = lock(&self.state);
        s.next_watch += 1;
        let id = s.next_watch;
        s.watchers.push((id, waker));
        Watch { id, state: Arc::downgrade(&self.state) }
    }

    /// While on, the next press is kept for [`Keyboard::take_heard`] (learn).
    pub fn learn(&self, on: bool) {
        let mut s = lock(&self.state);
        s.learning = on;
        s.heard = None;
    }

    pub fn take_heard(&self) -> Option<Key> {
        lock(&self.state).heard.take()
    }

    pub fn status(&self) -> Status {
        let s = lock(&self.state);
        let sources = |bits: u8| {
            [Source::Editor, Source::Viewer, Source::Output, Source::Command]
                .into_iter()
                .filter(|src| bits & src.bit() != 0)
                .collect::<Vec<_>>()
        };
        Status {
            wanted: self.wanted(),
            held: s
                .keys
                .iter()
                .filter(|(_, ks)| ks.holders != 0)
                .map(|(k, ks)| (k.name().to_string(), sources(ks.holders)))
                .collect(),
            last: s.last_key.map(|k| k.name().to_string()),
            presses: s.seq,
        }
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KEYBOARD_IN,
            "Keys of the keyboard as channels, to switch scenes or trigger things by hand during a show (while a penumbra window has the focus)",
        )
        .param(ParamSpec::text("keys", DEFAULT_KEYS).doc(
            "Space-separated keys: digits (the number pad's too), letters, f1 to f12, space, left right up \
             down, enter tab backspace delete insert home end pageup pagedown escape",
        ))
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`key_<name>` per key, 1 while held; `last`: the index in `keys` (from 0) of the key pressed last, \
             held until another listed key is pressed",
        ),
        || Box::new(KeyboardIn::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// Whether the network has a `chan.keyboard_in`.
pub fn wanted(net: &Network) -> bool {
    net.nodes().any(|n| n.kind == KEYBOARD_IN)
}

/// `keys` as keys, in order, with the names that are no key and the repeats left out (and
/// listed for a warning).
pub fn parse_keys(text: &str) -> (Vec<Key>, Vec<String>) {
    let mut keys: Vec<Key> = Vec::new();
    let mut bad = Vec::new();
    for token in text.split_whitespace() {
        match Key::parse(token) {
            Some(k) if keys.contains(&k) => bad.push(format!("`{token}` twice")),
            Some(_) if keys.len() >= MAX_KEYS => bad.push(format!("more than {MAX_KEYS} keys")),
            Some(k) => keys.push(k),
            None => bad.push(format!("`{token}` is no key")),
        }
    }
    (keys, bad)
}

/// The index in `keys` of the key pressed last (0 when none was).
pub fn last_index(views: &[KeyView]) -> usize {
    views.iter().enumerate().filter(|(_, v)| v.last > 0).max_by_key(|(_, v)| v.last).map_or(0, |(i, _)| i)
}

#[derive(Default)]
struct KeyboardIn {
    watch: Option<Watch>,
    /// Presses per key at the last cook, so a tap between two cooks still shows.
    seen: BTreeMap<Key, u64>,
    /// The values of the last cook.
    out: Vec<f64>,
}

impl Operator for KeyboardIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Keyboard>()
            .ok_or_else(|| CookError::failed("the keyboard is not available in this process"))?;
        if self.watch.is_none() {
            self.watch = Some(hub.watch(cx.waker()));
        }
        let (keys, bad) = parse_keys(cx.text("keys")?);
        let views = hub.view(&keys);
        let mut values = Vec::with_capacity(keys.len() + 1);
        let mut tapped = false;
        let mut seen = BTreeMap::new();
        for (k, v) in keys.iter().zip(&views) {
            // A key new to the list starts as it is: its earlier presses are no tap.
            let before = self.seen.get(k).copied().unwrap_or(v.presses);
            let tap = v.presses != before && !v.held;
            tapped |= tap;
            values.push(if v.held || tap { 1.0 } else { 0.0 });
            seen.insert(*k, v.presses);
        }
        self.seen = seen;
        // `as`: an index below MAX_KEYS is exact as f64.
        values.push(last_index(&views) as f64);
        if tapped {
            // Down again at the next frame.
            cx.waker().wake();
        }
        if !bad.is_empty() {
            cx.warn(bad.join("; "));
        }
        if values == self.out && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.out = values.clone();
        let mut channels = Vec::with_capacity(values.len());
        for (k, v) in keys.iter().zip(&values) {
            let name = Name::new(&k.channel()).map_err(|e| CookError::failed(e.0))?;
            channels.push(Channel { name, samples: vec![*v] });
        }
        let last = Name::new("last").map_err(|e| CookError::failed(e.0))?;
        channels.push(Channel { name: last, samples: vec![values.last().copied().unwrap_or(0.0)] });
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_and_aliases() {
        assert_eq!(Key::parse("1").map(Key::name), Some("1"));
        assert_eq!(Key::parse("Numpad3").map(Key::name), Some("3"));
        assert_eq!(Key::parse("num7").map(Key::name), Some("7"));
        assert_eq!(Key::parse("Digit0").map(Key::name), Some("0"));
        assert_eq!(Key::parse("A").map(Key::name), Some("a"));
        assert_eq!(Key::parse("KeyQ").map(Key::name), Some("q"));
        assert_eq!(Key::parse("F12").map(Key::name), Some("f12"));
        assert_eq!(Key::parse("f13"), None);
        assert_eq!(Key::parse("ArrowLeft").map(Key::name), Some("left"));
        assert_eq!(Key::parse("Space").map(Key::name), Some("space"));
        assert_eq!(Key::parse("Return").map(Key::name), Some("enter"));
        assert_eq!(Key::parse("numa"), None);
        assert_eq!(Key::parse("!"), None);
        assert_eq!(Key::parse(""), None);
        assert_eq!(Key::parse("keyboard"), None);
        for k in Key::all() {
            assert_eq!(Key::parse(k.name()), Some(k));
            assert!(Name::new(&k.channel()).is_ok(), "{}", k.channel());
        }
    }

    #[test]
    fn keys_param_keeps_order_and_drops_bad_names() {
        let (keys, bad) = parse_keys("1 2 x 2 bogus F1");
        assert_eq!(keys.iter().map(|k| k.name()).collect::<Vec<_>>(), ["1", "2", "x", "f1"]);
        assert_eq!(bad.len(), 2, "{bad:?}");
    }

    fn key(name: &str) -> Key {
        Key::parse(name).unwrap()
    }

    #[test]
    fn last_is_the_listed_key_pressed_last_and_holds() {
        let kb = Keyboard::new();
        let keys = [key("1"), key("2"), key("3"), key("4")];
        assert_eq!(last_index(&kb.view(&keys)), 0, "starts at 0");
        kb.tap(key("3"));
        assert_eq!(last_index(&kb.view(&keys)), 2);
        kb.tap(key("q"));
        assert_eq!(last_index(&kb.view(&keys)), 2, "a key not listed changes nothing");
        kb.tap(key("2"));
        kb.tap(key("4"));
        assert_eq!(last_index(&kb.view(&keys)), 3);
        kb.reset();
        assert_eq!(last_index(&kb.view(&keys)), 0, "a new project starts at 0");
    }

    #[test]
    fn holding_is_per_source_and_by_physical_key() {
        let kb = Keyboard::new();
        let one = key("1");
        kb.key(Source::Output, "Digit1", Some(one), true);
        kb.key(Source::Output, "Digit1", Some(one), true); // repeat
        assert_eq!(kb.view(&[one])[0], KeyView { held: true, presses: 1, last: 1 });
        // Down in the editor too: no second press; up there, still held by the output.
        kb.key(Source::Editor, "Num1", Some(one), true);
        kb.key(Source::Editor, "Num1", None, false);
        assert_eq!(kb.view(&[one])[0].presses, 1);
        assert!(kb.view(&[one])[0].held);
        // The release names the physical key; with Shift it means something else by then.
        kb.key(Source::Output, "Digit1", None, false);
        assert!(!kb.view(&[one])[0].held);
        // A window losing the focus lets go of what it held.
        kb.key(Source::Output, "KeyA", Some(key("a")), true);
        kb.release_all(Source::Output);
        assert!(!kb.view(&[key("a")])[0].held);
        // Nothing a key names: ignored.
        kb.key(Source::Editor, "Comma", None, true);
        assert_eq!(kb.status().presses, 2);
    }

    #[test]
    fn learn_keeps_the_next_press() {
        let kb = Keyboard::new();
        kb.tap(key("1"));
        kb.learn(true);
        assert_eq!(kb.take_heard(), None);
        kb.tap(key("f2"));
        assert_eq!(kb.take_heard(), Some(key("f2")));
        kb.learn(false);
        kb.tap(key("3"));
        assert_eq!(kb.take_heard(), None);
    }
}
