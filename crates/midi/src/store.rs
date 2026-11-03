//! What one device sent: the latest value per control, in the order the controls first came,
//! and the names they go by as channels and as OSC addresses.

use std::collections::BTreeMap;
use std::time::Instant;

use serde::Serialize;

use crate::message::Event;

/// One control of a device. Channels count from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Control {
    /// Control change `number` (a fader, knob or button).
    Cc {
        channel: u8,
        number: u8,
    },
    /// A key or pad.
    Note {
        channel: u8,
        note: u8,
    },
    Bend {
        channel: u8,
    },
    Pressure {
        channel: u8,
    },
    Program {
        channel: u8,
    },
    KeyPressure {
        channel: u8,
        note: u8,
    },
}

impl Control {
    /// The channel name: `cc_1_7`, `note_1_60`, `bend_1`, `pressure_1`, `program_1`,
    /// `key_pressure_1_60`.
    pub fn channel(&self) -> String {
        match *self {
            Control::Cc { channel, number } => format!("cc_{channel}_{number}"),
            Control::Note { channel, note } => format!("note_{channel}_{note}"),
            Control::Bend { channel } => format!("bend_{channel}"),
            Control::Pressure { channel } => format!("pressure_{channel}"),
            Control::Program { channel } => format!("program_{channel}"),
            Control::KeyPressure { channel, note } => format!("key_pressure_{channel}_{note}"),
        }
    }

    /// The part of the OSC address after `/midi/<device>`: `/cc/1/7`.
    pub fn path(&self) -> String {
        match *self {
            Control::Cc { channel, number } => format!("/cc/{channel}/{number}"),
            Control::Note { channel, note } => format!("/note/{channel}/{note}"),
            Control::Bend { channel } => format!("/bend/{channel}"),
            Control::Pressure { channel } => format!("/pressure/{channel}"),
            Control::Program { channel } => format!("/program/{channel}"),
            Control::KeyPressure { channel, note } => format!("/key_pressure/{channel}/{note}"),
        }
    }

    /// Reads a path as [`Control::path`] writes it.
    pub fn from_path(path: &str) -> Option<Control> {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let n = |i: usize, max: u8| -> Option<u8> {
            let v: u8 = parts.get(i)?.parse().ok()?;
            (v <= max).then_some(v)
        };
        let channel = || n(1, 16).filter(|c| *c >= 1);
        let c = match (parts.first().copied()?, parts.len()) {
            ("cc", 3) => Control::Cc { channel: channel()?, number: n(2, 127)? },
            ("note", 3) => Control::Note { channel: channel()?, note: n(2, 127)? },
            ("key_pressure", 3) => Control::KeyPressure { channel: channel()?, note: n(2, 127)? },
            ("bend", 2) => Control::Bend { channel: channel()? },
            ("pressure", 2) => Control::Pressure { channel: channel()? },
            ("program", 2) => Control::Program { channel: channel()? },
            _ => return None,
        };
        Some(c)
    }
}

/// The latest of one control.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub control: Control,
    /// 0 to 1 (a note: 1 while held; bend: -1 to 1; program: the number).
    pub value: f64,
    /// A note's velocity, 0 to 1, kept after it is let go.
    pub velocity: f64,
    /// Messages received for it.
    pub count: u64,
    /// A note's presses (note ons), so a tap shorter than a frame is not lost.
    pub presses: u64,
    #[serde(skip)]
    pub at: Option<Instant>,
}

/// One device's controls, the latest per control, in the order they first came.
#[derive(Clone, Debug, Default)]
pub struct Store {
    /// Changes with every message taken in.
    pub version: u64,
    pub entries: Vec<Entry>,
    index: BTreeMap<Control, usize>,
}

impl Store {
    fn entry(&mut self, control: Control) -> Option<&mut Entry> {
        let i = match self.index.get(&control) {
            Some(i) => *i,
            None => {
                // At most 16 x (128 + 128 + 128 + 3) controls exist, so the store is bounded.
                self.index.insert(control, self.entries.len());
                self.entries.push(Entry {
                    control,
                    value: 0.0,
                    velocity: 0.0,
                    count: 0,
                    presses: 0,
                    at: None,
                });
                self.entries.len() - 1
            }
        };
        self.entries.get_mut(i)
    }

    /// Sets `control` to `value`.
    pub fn set(&mut self, control: Control, value: f64, now: Instant) {
        if let Some(e) = self.entry(control) {
            e.value = value;
            e.count += 1;
            e.at = Some(now);
        }
        self.version += 1;
    }

    /// A note went down (`Some(velocity)`) or up (`None`).
    pub fn note(&mut self, control: Control, velocity: Option<f64>, now: Instant) {
        if let Some(e) = self.entry(control) {
            match velocity {
                Some(v) => {
                    e.value = 1.0;
                    e.velocity = v;
                    e.presses += 1;
                }
                None => e.value = 0.0,
            }
            e.count += 1;
            e.at = Some(now);
        }
        self.version += 1;
    }

    pub fn get(&self, control: &Control) -> Option<&Entry> {
        self.entries.get(*self.index.get(control)?)
    }
}

/// The value of a 7-bit data byte, 0 to 1.
pub fn seven(v: u8) -> f64 {
    f64::from(v.min(127)) / 127.0
}

/// Pitch bend, -1 to 1, 0 in the middle (8192).
pub fn bend(v: u16) -> f64 {
    let v = f64::from(v.min(16383)) - 8192.0;
    if v < 0.0 { v / 8192.0 } else { v / 8191.0 }
}

/// Pairs of control changes 0 to 31 (coarse) and 32 to 63 (fine) for one device: the fine
/// byte refines the value of the coarse one, as the MIDI spec has it for 14-bit controls.
#[derive(Clone, Debug, Default)]
pub struct Pairs {
    coarse: [[u8; 32]; 16],
}

impl Pairs {
    /// The control and value a control change makes: a coarse byte its value as 7 bits (a
    /// device that sends only those still reaches 1), a fine byte the pair's 14 bits.
    pub fn take(&mut self, channel: u8, number: u8, value: u8) -> (Control, f64) {
        let ch = usize::from(channel.saturating_sub(1));
        if let Some(slot) = self.coarse.get_mut(ch).and_then(|r| r.get_mut(usize::from(number))) {
            *slot = value;
            return (Control::Cc { channel, number }, seven(value));
        }
        if (32..64).contains(&number) {
            let n = number - 32;
            let coarse = self.coarse.get(ch).and_then(|r| r.get(usize::from(n))).copied().unwrap_or(0);
            let v = (u16::from(coarse.min(127)) << 7) | u16::from(value.min(127));
            return (Control::Cc { channel, number: n }, f64::from(v) / 16383.0);
        }
        (Control::Cc { channel, number }, seven(value))
    }
}

/// What a channel event does to the store: the control and its value, and whether a note went
/// down (its velocity) or up. `None` for clock and transport.
pub fn control_of(event: &Event, pairs: Option<&mut Pairs>) -> Option<(Control, f64, Option<Option<f64>>)> {
    Some(match *event {
        Event::Control { channel, number, value } => {
            let (c, v) = match pairs {
                Some(p) => p.take(channel, number, value),
                None => (Control::Cc { channel, number }, seven(value)),
            };
            (c, v, None)
        }
        Event::NoteOn { channel, note, velocity } => {
            (Control::Note { channel, note }, 1.0, Some(Some(seven(velocity))))
        }
        Event::NoteOff { channel, note } => (Control::Note { channel, note }, 0.0, Some(None)),
        Event::Bend { channel, value } => (Control::Bend { channel }, bend(value), None),
        Event::Pressure { channel, value } => (Control::Pressure { channel }, seven(value), None),
        Event::Program { channel, number } => (Control::Program { channel }, f64::from(number), None),
        Event::KeyPressure { channel, note, value } => {
            (Control::KeyPressure { channel, note }, seven(value), None)
        }
        Event::Clock | Event::Start | Event::Continue | Event::Stop | Event::SongPosition(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        let c = Control::Cc { channel: 1, number: 7 };
        assert_eq!((c.channel().as_str(), c.path().as_str()), ("cc_1_7", "/cc/1/7"));
        for c in [
            c,
            Control::Note { channel: 16, note: 127 },
            Control::Bend { channel: 3 },
            Control::Pressure { channel: 2 },
            Control::Program { channel: 9 },
            Control::KeyPressure { channel: 1, note: 0 },
        ] {
            assert_eq!(Control::from_path(&c.path()), Some(c));
        }
        for bad in ["/cc/0/7", "/cc/17/7", "/cc/1/128", "/cc/1", "/note/1/2/3", "/x/1/2", ""] {
            assert_eq!(Control::from_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn fourteen_bit_pairs() {
        let mut p = Pairs::default();
        assert_eq!(p.take(1, 7, 127), (Control::Cc { channel: 1, number: 7 }, 1.0));
        let (c, v) = p.take(1, 39, 127);
        assert_eq!((c, v), (Control::Cc { channel: 1, number: 7 }, 1.0));
        p.take(1, 7, 64);
        let (_, v) = p.take(1, 39, 0);
        assert!((v - 8192.0 / 16383.0).abs() < 1e-12);
        // Above 63 nothing pairs.
        assert_eq!(p.take(2, 70, 0), (Control::Cc { channel: 2, number: 70 }, 0.0));
        assert_eq!((bend(0), bend(8192), bend(16383)), (-1.0, 0.0, 1.0));
    }
}
