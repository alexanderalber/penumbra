//! MIDI nodes.
//!
//! - **`chan.midi_in`** reads a MIDI controller (a device by a part of its name, or every
//!   input) and makes every control it moved a channel holding its latest value: `cc_1_7` for
//!   control change 7 on MIDI channel 1 (0 to 1), `note_1_36` for a key or pad (1 while held;
//!   a tap shorter than a frame still shows for one frame) with `note_1_36_vel` its velocity,
//!   `bend_1` (-1 to 1), `pressure_1`, `program_1`. MIDI clock from the device becomes a tempo
//!   with the same channels as `chan.beat_analysis` (`bpm`, `beat`, `tick`, `bar`,
//!   `bar_beat`), plus `beats`, `playing` and `clock`. Learn binds a param to the next control
//!   moved (the engine does that, through the OSC hub). The node recooks when something
//!   arrives, and every frame while the clock plays.
//! - **`chan.midi_out`** sends its input's channels to a device, named as `chan.midi_in` makes
//!   them (`cc_1_7`, `note_1_36`; `cc_7` and `note_36` on the node's `channel`): pad lights,
//!   motor faders, another program. Only what changed is sent, and everything again when the
//!   device is opened anew.
//!
//! The devices belong to the engine's [`Hub`], which opens what [`wants`] lists after every
//! change of the network.

use std::collections::BTreeMap;
use std::sync::Arc;

use penumbra_core::chan::MAX_CHANNELS;
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use penumbra_midi::hub::{Hub, Want, Watch, matches};
use penumbra_midi::{Control, Event, Snapshot, encode};

use crate::osc::Made;

/// Kind of the MIDI input node.
pub const MIDI_IN: &str = "chan.midi_in";
/// Kind of the MIDI output node.
pub const MIDI_OUT: &str = "chan.midi_out";

/// The clock's channels, in their order.
pub const CLOCK_CHANNELS: [&str; 8] = ["bpm", "beat", "tick", "bar", "bar_beat", "beats", "playing", "clock"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            MIDI_IN,
            "A MIDI controller's faders, knobs, pads and keys, and its MIDI clock as a tempo: every control a channel",
        )
        .block(penumbra_graph::Block::In)
        .param(ParamSpec::text("device", "").doc(
            "Part of the device's name, any case, e.g. `easy control` (the … menu lists them); empty: every MIDI input",
        ))
        .param(
            ParamSpec::int("beats_per_bar", 4)
                .clamp(1.0, 64.0)
                .range(2.0, 8.0)
                .doc("Beats in one bar of the MIDI clock: how often bar and bar_beat come round"),
        )
        .param(ParamSpec::bool("pairs", false).more().doc(
            "14-bit controls: control changes 32 to 63 make the controls 0 to 31 finer instead of being controls of their own. Only for a device that sends such pairs",
        ))
        .output("out", DataKind::Chan)
        .output("messages", DataKind::Table)
        .describe_output(
            "out",
            "A channel per control moved: cc_1_7 (control change 7 on MIDI channel 1, 0 to 1), note_1_36 (1 while held) and note_1_36_vel, bend_1 (-1 to 1), pressure_1, program_1. With MIDI clock: bpm, beat, tick, bar, bar_beat as chan.beat_analysis has them, beats (since the start), playing, clock (1 while clocks arrive)",
        )
        .describe_output("messages", "Every control moved: device, channel name, count, value"),
        || Box::new(MidiIn::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            MIDI_OUT,
            "Sends channels to a MIDI device: pad lights, motor faders, another program",
        )
        .param(ParamSpec::text("device", "").doc(
            "Part of the output's name, any case, e.g. `easy control` (the … menu lists them)",
        ))
        .param(
            ParamSpec::int("channel", 1)
                .clamp(1.0, 16.0)
                .doc("MIDI channel for channels named without one: cc_7, note_36"),
        )
        .param(
            ParamSpec::float("rate", 60.0)
                .clamp(1.0, 1000.0)
                .range(1.0, 120.0)
                .unit(Unit::Hertz)
                .more()
                .doc("Sends per second at most"),
        )
        .input("chans", DataKind::Chan)
        .describe_input(
            "chans",
            "Channels named as chan.midi_in makes them: cc_1_7 (0 to 1), note_1_36 (above 0 is on, its value the velocity, or note_1_36_vel's when that channel is there too), bend_1 (-1 to 1), pressure_1, program_1; cc_7, note_36 and so on use `channel`. A chan.midi_in wired straight in sends its controls back",
        )
        .time_dependent(),
        || Box::new(MidiOut::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The inputs and outputs the network's MIDI nodes want (stored params).
pub fn wants(net: &Network, types: &NodeTypes) -> (Vec<Want>, Vec<String>) {
    let text = |kind: &str, n: &penumbra_graph::NodeDef, name: &str| -> String {
        types
            .desc(kind)
            .and_then(|d| n.param(d, name))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for n in net.nodes() {
        if n.kind == MIDI_IN {
            let pairs = types.desc(MIDI_IN).and_then(|d| n.param(d, "pairs")).and_then(Value::as_bool);
            inputs.push(Want { device: text(MIDI_IN, n, "device"), pairs: pairs.unwrap_or(false) });
        } else if n.kind == MIDI_OUT {
            let d = text(MIDI_OUT, n, "device");
            if !d.is_empty() {
                outputs.push(d);
            }
        }
    }
    (inputs, outputs)
}

fn hub_of<'a>(cx: &'a CookContext<'_>) -> Result<&'a Hub, CookError> {
    cx.resource::<Hub>().ok_or_else(|| CookError::failed("MIDI is not available in this process"))
}

/// Why nothing reads: no such device, or it is not open yet.
fn missing(hub: &Hub, pattern: &str, outputs: bool) -> CookError {
    if !hub.config().enabled {
        return CookError::failed("MIDI is switched off (midi.config enabled)");
    }
    let ports = hub.ports();
    if let Some(e) = ports.error {
        return CookError::failed(e);
    }
    let list = if outputs { &ports.outputs } else { &ports.inputs };
    let what = if outputs { "output" } else { "input" };
    if let Some(name) = list.iter().find(|p| matches(pattern, p)) {
        return CookError::failed(format!("opening {name}"));
    }
    if list.is_empty() {
        return CookError::failed(format!("this machine has no MIDI {what}: plug the device in"));
    }
    CookError::failed(format!("no MIDI {what} matches `{}`; there are: {}", pattern.trim(), list.join(", ")))
}

/// A note's presses as the node last saw them, so a tap within one frame still shows.
#[derive(Clone, Copy, Debug, Default)]
struct NoteSeen {
    presses: u64,
    /// The gate was held up for a tap at the last cook: the next shows the note as it is.
    pulsed: bool,
}

/// Per input its store version and whether it was connected; the device and beats per bar.
type Seen = (Vec<(u64, bool)>, String, i64);

#[derive(Default)]
struct MidiIn {
    watch: Option<Watch>,
    /// The store versions, whether each input was connected, and the params of the last cook.
    seen: Option<Seen>,
    notes: BTreeMap<(String, Control), NoteSeen>,
    /// The whole beat at the last cook, for `tick`.
    last_beat: Option<i64>,
}

fn fract(x: f64) -> f64 {
    x - x.floor()
}

impl Operator for MidiIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        if self.watch.is_none() {
            let waker = cx.waker();
            self.watch = Some(hub.watch(Arc::new(move || waker.wake())));
        }
        if let Some(w) = &self.watch {
            w.ack();
        }
        let pattern = cx.text("device")?.to_string();
        let per_bar = cx.int("beats_per_bar")?.clamp(1, 64);
        let inputs: Vec<Snapshot> = hub.inputs(&pattern);
        if inputs.is_empty() {
            return Err(missing(hub, &pattern, false));
        }
        // An input unplugged or back changes the warning, not the store.
        let versions: Vec<(u64, bool)> = inputs.iter().map(|s| (s.store.version, s.connected)).collect();
        let clock = inputs.iter().find(|s| s.clock.clocks > 0).map(|s| s.clock);
        let pulsing = self.notes.values().any(|n| n.pulsed);
        let seen = (versions, pattern.clone(), per_bar);
        if !cx.params_changed()
            && !cx.first_cook()
            && !pulsing
            && clock.is_none_or(|c| !c.playing)
            && self.seen.as_ref() == Some(&seen)
        {
            return Ok(Cooked::Unchanged);
        }
        self.seen = Some(seen);
        // The clock's channels always fit: the devices' stop short of them.
        let mut devices = Made::new(MAX_CHANNELS - if clock.is_some() { CLOCK_CHANNELS.len() } else { 0 });
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut clashes = Vec::new();
        let mut rows =
            vec![vec!["device".to_string(), "channel".to_string(), "count".to_string(), "value".to_string()]];
        let mut push = |name: String, value: f64, device: &str, made: &mut Made| {
            if let Some(first) = taken.get(&name) {
                if first != device {
                    clashes.push(format!("{device} and {first} both make `{name}`"));
                }
                return;
            }
            let Ok(n) = Name::new(&name) else { return };
            if made.full() {
                return;
            }
            taken.insert(name, device.to_string());
            made.channels
                .push(Channel { name: n, samples: vec![if value.is_finite() { value } else { 0.0 }] });
        };
        let mut again = false;
        for input in &inputs {
            for e in &input.store.entries {
                let name = e.control.channel();
                rows.push(vec![
                    input.name.clone(),
                    name.clone(),
                    e.count.to_string(),
                    format!("{}", e.value),
                ]);
                if let Control::Note { .. } = e.control {
                    let seen = self.notes.entry((input.name.clone(), e.control)).or_default();
                    let mut gate = e.value;
                    if e.presses > seen.presses && !seen.pulsed && gate < 0.5 {
                        // Pressed and let go since the last cook: up for this frame.
                        gate = 1.0;
                        seen.pulsed = true;
                        again = true;
                    } else {
                        seen.pulsed = false;
                    }
                    seen.presses = e.presses;
                    push(name.clone(), gate, &input.name, &mut devices);
                    push(format!("{name}_vel"), e.velocity, &input.name, &mut devices);
                } else {
                    push(name, e.value, &input.name, &mut devices);
                }
            }
        }
        if let Some(c) = clock {
            let beats = c.beats;
            let whole = beats.floor();
            // `as`: a beat count far below 2^63.
            let whole_i = whole as i64;
            let tick = c.playing && self.last_beat.is_some_and(|b| whole_i > b);
            self.last_beat = Some(whole_i);
            let bars = beats / per_bar as f64;
            let values = [
                c.bpm,
                fract(beats),
                if tick { 1.0 } else { 0.0 },
                fract(bars),
                (whole_i.rem_euclid(per_bar)) as f64,
                beats,
                if c.playing { 1.0 } else { 0.0 },
                if c.ticking { 1.0 } else { 0.0 },
            ];
            let mut beat = Made::new(MAX_CHANNELS);
            for (name, v) in CLOCK_CHANNELS.iter().zip(values) {
                push((*name).to_string(), v, "the clock", &mut beat);
            }
            devices.channels.extend(beat.channels);
            // The beat glides between clocks, and `tick` falls again at the next frame.
            again |= c.playing || tick;
        }
        if again {
            cx.waker().wake();
        }
        let mut warnings: Vec<String> = inputs
            .iter()
            .filter(|s| !s.connected)
            .map(|s| format!("{} is unplugged: its channels hold their last values", s.name))
            .collect();
        if let Some(c) = clashes.first() {
            warnings.push(format!("{c}; the first is kept"));
        }
        warnings.extend(devices.warning("`device`"));
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let chans = Chans::new(cx.rate().as_f64(), devices.channels)?;
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Chan, chans),
            Data::new(DataKind::Table, Table::new(rows)?),
        ]))
    }
}

/// What a channel named `name` with `value` sends, on MIDI channel `default` when the name
/// has none; `None` for a name that is no MIDI control. A note's velocity channel
/// (`note_1_36_vel`) and the clock's channels send nothing, so a `chan.midi_in` can be wired
/// straight in.
pub fn event_of(name: &str, value: f64, default: u8) -> Option<Option<(Control, Event)>> {
    let v = if value.is_finite() { value } else { 0.0 };
    // `as`: clamped into 0 to 127 (or 16383) first.
    let seven = |x: f64| (x.clamp(0.0, 1.0) * 127.0).round() as u8;
    if name.ends_with("_vel") || CLOCK_CHANNELS.contains(&name) {
        return Some(None);
    }
    let parts: Vec<&str> = name.split('_').collect();
    let (kind, rest): (&str, &[&str]) = match parts.as_slice() {
        ["key", "pressure", rest @ ..] => ("key_pressure", rest),
        [kind, rest @ ..] => (kind, rest),
        [] => return None,
    };
    let nums: Option<Vec<u8>> = rest.iter().map(|p| p.parse::<u8>().ok()).collect();
    let nums = nums?;
    let (channel, number) = match (kind, nums.as_slice()) {
        ("cc" | "note" | "key_pressure", [c, n]) => (*c, Some(*n)),
        ("cc" | "note" | "key_pressure", [n]) => (default, Some(*n)),
        ("bend" | "pressure" | "program", [c]) => (*c, None),
        ("bend" | "pressure" | "program", []) => (default, None),
        _ => return None,
    };
    if !(1..=16).contains(&channel) || number.is_some_and(|n| n > 127) {
        return None;
    }
    let n = number.unwrap_or(0);
    Some(Some(match kind {
        "cc" => (Control::Cc { channel, number: n }, Event::Control { channel, number: n, value: seven(v) }),
        "note" => {
            // Velocity 0 is a note off; lights take it as "off".
            let velocity = if v > 0.0 { seven(v).max(1) } else { 0 };
            (Control::Note { channel, note: n }, Event::NoteOn { channel, note: n, velocity })
        }
        "key_pressure" => (
            Control::KeyPressure { channel, note: n },
            Event::KeyPressure { channel, note: n, value: seven(v) },
        ),
        "bend" => {
            // `as`: clamped into 0 to 16383 first.
            let value = ((v.clamp(-1.0, 1.0) + 1.0) * 8191.5).round().clamp(0.0, 16383.0) as u16;
            (Control::Bend { channel }, Event::Bend { channel, value })
        }
        "pressure" => (Control::Pressure { channel }, Event::Pressure { channel, value: seven(v) }),
        // `as`: clamped into 0 to 127 first.
        _ => (
            Control::Program { channel },
            Event::Program { channel, number: v.round().clamp(0.0, 127.0) as u8 },
        ),
    }))
}

#[derive(Default)]
struct MidiOut {
    /// The output and its generation the `sent` map belongs to.
    target: Option<(String, u64)>,
    /// Show time of the last send, in seconds.
    last: Option<f64>,
    /// What was sent last, per control.
    sent: BTreeMap<Control, Vec<u8>>,
}

impl Operator for MidiOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let pattern = cx.text("device")?.trim().to_string();
        if pattern.is_empty() {
            return Err(CookError::failed("name the device in `device` (the … menu lists them)"));
        }
        let Some(name) = hub.output_for(&pattern) else { return Err(missing(hub, &pattern, true)) };
        let status = hub.output(&name);
        let Some(status) = status.filter(|s| s.connected) else {
            return Err(match hub.output(&name).and_then(|s| s.error) {
                Some(e) => CookError::failed(format!("{name}: {e}")),
                None => CookError::failed(format!("opening {name}")),
            });
        };
        let target = (name.clone(), status.generation);
        if self.target.as_ref() != Some(&target) || cx.params_changed() {
            // A device opened anew forgot what it showed: everything goes again.
            self.sent.clear();
            self.target = Some(target);
        }
        let Some(chans) = cx.input_as::<Chans>(0) else { return Err(crate::waiting(cx)) };
        let now = cx.time().seconds();
        let period = 1.0 / cx.float("rate")?.max(1.0);
        if let Some(last) = self.last
            && now >= last
            && now - last < period * 0.9
            && !self.sent.is_empty()
        {
            return Ok(Cooked::Outputs(Vec::new()));
        }
        self.last = Some(now);
        let default = u8::try_from(cx.int("channel")?.clamp(1, 16)).unwrap_or(1);
        let mut unknown = Vec::new();
        let mut full = false;
        // A note held with a velocity channel beside it (as chan.midi_in makes them) goes with
        // that velocity.
        let velocities: BTreeMap<String, f64> = chans
            .channels()
            .iter()
            .filter_map(|ch| Some((ch.name.as_str().strip_suffix("_vel")?.to_string(), ch.value())))
            .collect();
        for ch in chans.channels() {
            let name = ch.name.to_string();
            let mut value = ch.value();
            if name.starts_with("note_")
                && value > 0.0
                && let Some(v) = velocities.get(&name).filter(|v| **v > 0.0)
            {
                value = *v;
            }
            match event_of(&name, value, default) {
                None => unknown.push(name),
                Some(None) => {}
                Some(Some((control, event))) => {
                    let Some(bytes) = encode(&event) else { continue };
                    if self.sent.get(&control) == Some(&bytes) {
                        continue;
                    }
                    if hub.send(&target_name(&self.target), bytes.clone()) {
                        self.sent.insert(control, bytes);
                    } else {
                        full = true;
                    }
                }
            }
        }
        let mut warnings = Vec::new();
        if let Some(first) = unknown.first() {
            warnings.push(format!(
                "{} channels are no MIDI control and are not sent, e.g. `{first}` (cc_1_7, note_36, bend_1 are)",
                unknown.len()
            ));
        }
        if full {
            warnings.push("the MIDI queue is full: some messages wait for the next frame".to_string());
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

fn target_name(target: &Option<(String, u64)>) -> String {
    target.as_ref().map(|(n, _)| n.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_become_messages() {
        let ev = |n: &str, v: f64| event_of(n, v, 2).flatten().map(|(_, e)| e);
        assert_eq!(ev("cc_1_7", 1.0), Some(Event::Control { channel: 1, number: 7, value: 127 }));
        assert_eq!(ev("cc_7", 0.5), Some(Event::Control { channel: 2, number: 7, value: 64 }));
        assert_eq!(ev("note_36", 0.0), Some(Event::NoteOn { channel: 2, note: 36, velocity: 0 }));
        assert_eq!(ev("note_10_36", 0.001), Some(Event::NoteOn { channel: 10, note: 36, velocity: 1 }));
        assert_eq!(ev("bend_1", 0.0), Some(Event::Bend { channel: 1, value: 8192 }));
        assert_eq!(ev("bend", 1.0), Some(Event::Bend { channel: 2, value: 16383 }));
        assert_eq!(ev("program_3", 5.2), Some(Event::Program { channel: 3, number: 5 }));
        assert_eq!(
            ev("key_pressure_1_60", 1.0),
            Some(Event::KeyPressure { channel: 1, note: 60, value: 127 })
        );
        // Sent by nothing, but no warning: what a chan.midi_in makes besides its controls.
        assert_eq!(event_of("note_1_36_vel", 1.0, 1), Some(None));
        assert_eq!(event_of("bpm", 120.0, 1), Some(None));
        for bad in ["fader1", "cc_17_1", "cc_1_200", "cc", "note_a", "cc_1_2_3"] {
            assert_eq!(event_of(bad, 1.0, 1), None, "{bad}");
        }
    }
}
