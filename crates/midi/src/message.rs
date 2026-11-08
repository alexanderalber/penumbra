//! MIDI messages: the bytes a device sends, read into [`Event`]s, and what `out.midi`
//! sends, written back into bytes.
//!
//! The reader takes whatever a driver hands over (one message per call on WinMM and CoreMIDI,
//! a stream with running status elsewhere) and never trusts a length: a data byte without a
//! status before it, a message cut short, or a system exclusive message is skipped and
//! counted. Channels are 1 to 16 here, as devices and manuals number them.

use serde::Serialize;

/// One thing a device said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Event {
    /// A key or pad went down. A note on with velocity 0 is read as [`Event::NoteOff`].
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
    },
    /// A fader, knob or button: control change.
    Control {
        channel: u8,
        number: u8,
        value: u8,
    },
    /// Pitch bend, 0 to 16383, 8192 in the middle.
    Bend {
        channel: u8,
        value: u16,
    },
    /// Channel pressure (aftertouch for the whole channel).
    Pressure {
        channel: u8,
        value: u8,
    },
    /// Program change.
    Program {
        channel: u8,
        number: u8,
    },
    /// Pressure of one key (polyphonic aftertouch).
    KeyPressure {
        channel: u8,
        note: u8,
        value: u8,
    },
    /// MIDI clock: 24 per quarter note.
    Clock,
    Start,
    Continue,
    Stop,
    /// Song position in sixteenth notes (6 clocks each).
    SongPosition(u16),
}

/// Reads a stream of MIDI bytes into events, keeping running status between calls.
#[derive(Clone, Debug, Default)]
pub struct Reader {
    /// The status byte of the channel message in progress (running status).
    status: Option<u8>,
    data: [u8; 2],
    have: usize,
    /// Inside a system exclusive message: bytes until its end are skipped.
    sysex: bool,
    /// Bytes skipped: data without a status, unknown or cut short messages, system exclusive.
    pub skipped: u64,
}

/// Data bytes a message with this status takes, `None` for one this reader skips.
fn data_len(status: u8) -> Option<usize> {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => Some(2),
        0xC0 | 0xD0 => Some(1),
        _ => match status {
            0xF2 => Some(2),
            0xF1 | 0xF3 => Some(1),
            _ => None,
        },
    }
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `bytes`, calling `out` for every whole message in them.
    pub fn read(&mut self, bytes: &[u8], out: &mut dyn FnMut(Event)) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut dyn FnMut(Event)) {
        if b >= 0xF8 {
            // Real-time bytes may come anywhere, even inside another message.
            match b {
                0xF8 => out(Event::Clock),
                0xFA => out(Event::Start),
                0xFB => out(Event::Continue),
                0xFC => out(Event::Stop),
                // Active sensing, reset and the undefined ones say nothing to a show.
                _ => {}
            }
            return;
        }
        if b >= 0x80 {
            if self.sysex && b != 0xF7 {
                // A status inside system exclusive ends it.
                self.sysex = false;
            }
            if self.have > 0 {
                // The message before was cut short.
                self.skipped += 1;
            }
            self.have = 0;
            match b {
                0xF0 => {
                    self.sysex = true;
                    self.status = None;
                    self.skipped += 1;
                }
                0xF7 => {
                    self.sysex = false;
                    self.status = None;
                }
                _ => match data_len(b) {
                    Some(_) => self.status = Some(b),
                    None => {
                        // Tune request and the undefined system bytes: no data, nothing to do.
                        self.status = None;
                        self.skipped += 1;
                    }
                },
            }
            return;
        }
        // A data byte.
        if self.sysex {
            return;
        }
        let Some(status) = self.status else {
            self.skipped += 1;
            return;
        };
        let Some(len) = data_len(status) else {
            self.skipped += 1;
            return;
        };
        if let Some(slot) = self.data.get_mut(self.have) {
            *slot = b;
        }
        self.have += 1;
        if self.have < len {
            return;
        }
        self.have = 0;
        let [d0, d1] = self.data;
        let channel = (status & 0x0F) + 1;
        let event = match status & 0xF0 {
            0x80 => Event::NoteOff { channel, note: d0 },
            0x90 if d1 == 0 => Event::NoteOff { channel, note: d0 },
            0x90 => Event::NoteOn { channel, note: d0, velocity: d1 },
            0xA0 => Event::KeyPressure { channel, note: d0, value: d1 },
            0xB0 => Event::Control { channel, number: d0, value: d1 },
            0xC0 => Event::Program { channel, number: d0 },
            0xD0 => Event::Pressure { channel, value: d0 },
            0xE0 => Event::Bend { channel, value: u16::from(d0) | (u16::from(d1) << 7) },
            _ => {
                // System common: no running status.
                self.status = None;
                match status {
                    0xF2 => Event::SongPosition(u16::from(d0) | (u16::from(d1) << 7)),
                    // MIDI time code quarter frames and song select: not read.
                    _ => return,
                }
            }
        };
        out(event);
    }
}

/// The bytes of a channel message, `None` for a channel outside 1 to 16. Values are clamped
/// into their 7 or 14 bits.
pub fn encode(event: &Event) -> Option<Vec<u8>> {
    let status = |kind: u8, channel: u8| -> Option<u8> {
        if (1..=16).contains(&channel) { Some(kind | (channel - 1)) } else { None }
    };
    let b = |v: u8| v.min(127);
    Some(match *event {
        Event::NoteOn { channel, note, velocity } => vec![status(0x90, channel)?, b(note), b(velocity)],
        Event::NoteOff { channel, note } => vec![status(0x80, channel)?, b(note), 0],
        Event::Control { channel, number, value } => vec![status(0xB0, channel)?, b(number), b(value)],
        Event::Bend { channel, value } => {
            let v = value.min(16383);
            // `as`: both parts are 7 bits.
            vec![status(0xE0, channel)?, (v & 0x7F) as u8, (v >> 7) as u8]
        }
        Event::Pressure { channel, value } => vec![status(0xD0, channel)?, b(value)],
        Event::Program { channel, number } => vec![status(0xC0, channel)?, b(number)],
        Event::KeyPressure { channel, note, value } => vec![status(0xA0, channel)?, b(note), b(value)],
        Event::Clock => vec![0xF8],
        Event::Start => vec![0xFA],
        Event::Continue => vec![0xFB],
        Event::Stop => vec![0xFC],
        Event::SongPosition(p) => {
            let p = p.min(16383);
            // `as`: both parts are 7 bits.
            vec![0xF2, (p & 0x7F) as u8, (p >> 7) as u8]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bytes: &[u8]) -> (Vec<Event>, u64) {
        let mut r = Reader::new();
        let mut out = Vec::new();
        r.read(bytes, &mut |e| out.push(e));
        (out, r.skipped)
    }

    #[test]
    fn channel_messages_with_running_status() {
        let (ev, skipped) = read(&[0xB0, 7, 100, 8, 50, 0x91, 60, 127, 60, 0, 0xE2, 0, 64]);
        assert_eq!(
            ev,
            [
                Event::Control { channel: 1, number: 7, value: 100 },
                Event::Control { channel: 1, number: 8, value: 50 },
                Event::NoteOn { channel: 2, note: 60, velocity: 127 },
                Event::NoteOff { channel: 2, note: 60 },
                Event::Bend { channel: 3, value: 8192 },
            ]
        );
        assert_eq!(skipped, 0);
    }

    #[test]
    fn real_time_bytes_inside_a_message_and_sysex_skipped() {
        let (ev, skipped) = read(&[0xB0, 7, 0xF8, 100, 0xF0, 1, 2, 3, 0xF7, 0xFA, 0xC0, 5]);
        assert_eq!(
            ev,
            [
                Event::Clock,
                Event::Control { channel: 1, number: 7, value: 100 },
                Event::Start,
                Event::Program { channel: 1, number: 5 }
            ]
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn garbage_is_counted_not_trusted() {
        // Data without status, a message cut short by another status, undefined system bytes.
        let (ev, skipped) = read(&[5, 6, 0x90, 60, 0xB0, 1, 2, 0xF4, 9, 0xF2, 0x10, 0x01]);
        assert_eq!(ev, [Event::Control { channel: 1, number: 1, value: 2 }, Event::SongPosition(0x90)]);
        assert_eq!(skipped, 5);
    }

    #[test]
    fn encode_reads_back() {
        let events = [
            Event::NoteOn { channel: 16, note: 36, velocity: 99 },
            Event::Control { channel: 1, number: 20, value: 0 },
            Event::Bend { channel: 4, value: 12345 },
            Event::Program { channel: 9, number: 3 },
            Event::Stop,
        ];
        let bytes: Vec<u8> = events.iter().filter_map(encode).flatten().collect();
        assert_eq!(read(&bytes).0, events);
        assert_eq!(encode(&Event::Control { channel: 0, number: 1, value: 1 }), None);
        assert_eq!(
            encode(&Event::Control { channel: 1, number: 200, value: 255 }),
            Some(vec![0xB0, 127, 127])
        );
    }
}
