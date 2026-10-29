//! What `audio.instrument` and `audio.ambience` share: channels as lanes whose threshold
//! crossings are timed inside the frame, and the musical params (key, scale, octave, note,
//! range, spread, brightness, level).
//!
//! A lane's crossing time is interpolated linearly between its samples: its value at the end of
//! the previous cook and every newer sample of this cook. The last sample of a channel is now;
//! sample `j` from the end lies `j / rate` earlier. A channel with one sample per frame so has
//! its previous value at the block's start. Samples older than the previous cook were seen then
//! and are skipped. A lane with no history (the first cook, after a seek, a new channel) only
//! records its value.

use std::collections::BTreeMap;

use penumbra_core::{Chans, Curve, ParamSpec, SampleAxis, Unit};
use penumbra_graph::{CookContext, CookError};

use super::block::Span;
use super::pitch::{KEYS, SCALES, Tuning};

/// The quietest `level`, dB: this and below is silence.
pub const MIN_LEVEL_DB: f64 = -100.0;
pub const MAX_LEVEL_DB: f64 = 24.0;

/// Which crossings count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Both,
}

impl Direction {
    pub fn parse(s: &str) -> Self {
        match s {
            "down" => Self::Down,
            "both" => Self::Both,
            _ => Self::Up,
        }
    }
}

/// A lane's value and the time of that value, seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub time: f64,
    pub value: f64,
}

/// One crossing: when (seconds) and whether the value rose above the threshold (else it fell
/// to it or below).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crossing {
    pub time: f64,
    pub rising: bool,
}

/// The crossings of `threshold` this way, pushed to `out`: between `prev` and the samples
/// newer than it, the last of which is at `now`, spaced `1 / rate` (`None`: only the last
/// sample counts). A value on the threshold counts as below it, so a pulse from 0 to 1 with
/// threshold 0 rises at its 0 and falls at its 0 again. Rising and falling crossings alternate.
/// Returns the lane's newest value.
pub fn crossings(
    prev: Option<Seen>,
    samples: &[f64],
    rate: Option<f64>,
    now: f64,
    threshold: f64,
    dir: Direction,
    out: &mut Vec<Crossing>,
) -> Option<Seen> {
    let len = samples.len();
    let newest = samples.last().map(|&value| Seen { time: now, value })?;
    let Some(mut last) = prev else { return Some(newest) };
    let step = rate.filter(|r| r.is_finite() && *r > 0.0).map(|r| 1.0 / r);
    let first = if step.is_some() { 0 } else { len - 1 };
    for (i, &value) in samples.iter().enumerate().skip(first) {
        let time = match step {
            Some(s) => now - (len - 1 - i) as f64 * s,
            None => now,
        };
        if time <= last.time + 1e-9 {
            continue;
        }
        let rising = last.value <= threshold && value > threshold;
        let falling = last.value > threshold && value <= threshold;
        let wanted = match dir {
            Direction::Up => rising,
            Direction::Down => falling,
            Direction::Both => rising || falling,
        };
        if wanted {
            let frac = ((threshold - last.value) / (value - last.value)).clamp(0.0, 1.0);
            out.push(Crossing { time: last.time + frac * (time - last.time), rising });
        }
        last = Seen { time, value };
    }
    Some(newest)
}

/// An event of one lane at a sample of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Event {
    pub sample: i64,
    pub lane: usize,
    pub rising: bool,
}

/// The crossings of every channel of `input` as events inside `span`, sorted by sample then
/// lane; `lanes` holds each channel's value by name and is replaced by this cook's.
#[allow(clippy::too_many_arguments)]
pub fn events(
    input: Option<&Chans>,
    lanes: &mut BTreeMap<String, Seen>,
    now: f64,
    rate: f64,
    span: &Span,
    threshold: f64,
    dir: Direction,
    scratch: &mut Vec<Crossing>,
    out: &mut Vec<Event>,
) {
    out.clear();
    let mut next = BTreeMap::new();
    if let Some(chans) = input {
        let chan_rate = (chans.axis() == SampleAxis::Time).then(|| chans.rate());
        for (lane, c) in chans.channels().iter().enumerate() {
            scratch.clear();
            let prev = lanes.get(c.name.as_str()).copied();
            if let Some(seen) = crossings(prev, &c.samples, chan_rate, now, threshold, dir, scratch) {
                next.insert(c.name.as_str().to_string(), seen);
            }
            for x in scratch.iter() {
                let s = (x.time * rate).round();
                let s = if s.is_finite() { s as i64 } else { span.start };
                let sample = s.clamp(span.start, (span.end - 1).max(span.start));
                out.push(Event { sample, lane, rising: x.rising });
            }
        }
    }
    *lanes = next;
    out.sort();
}

/// The params both nodes share, in their order: key, scale, octave, note, range, spread.
pub fn pitch_params(note_doc: &str, spread_doc: &str) -> [ParamSpec; 6] {
    let mut key = ParamSpec::choice("key", &KEYS).doc("The root note of the scale: note 0 plays it");
    for (i, k) in KEYS.iter().enumerate() {
        let hz = super::pitch::hertz(60.0 + i as f64);
        key = key.choice_doc(k, &format!("the root is {k}, {hz:.1} Hz in octave 4"));
    }
    [
        key,
        ParamSpec::choice("scale", &SCALES)
            .doc("The notes that can be played. With a scale, every note fits with every other")
            .choice_doc(
                "major_pentatonic",
                "five notes per octave (C D E G A in C): any notes together sound fine",
            )
            .choice_doc("minor_pentatonic", "five notes per octave (C Eb F G Bb in C): darker, bluesy")
            .choice_doc("major", "seven notes per octave, the white keys from C in C")
            .choice_doc("minor", "seven notes per octave, the white keys from A in A")
            .choice_doc("chromatic", "all twelve semitones")
            .choice_doc(
                "off",
                "free pitch: note moves smoothly between the notes, as a theremin. spread counts semitones",
            ),
        ParamSpec::int("octave", 4)
            .clamp(-1.0, 9.0)
            .range(0.0, 8.0)
            .doc("The octave of the root that note 0 plays: 4 is the octave of middle C, where A is 440 Hz"),
        ParamSpec::float("note", 0.0).clamp(-10.0, 10.0).range(0.0, 1.0).doc(note_doc),
        ParamSpec::float("range", 2.0)
            .clamp(0.0, 10.0)
            .range(0.0, 4.0)
            .more()
            .doc("Octaves that note 0 to 1 spans"),
        ParamSpec::int("spread", 1).clamp(-128.0, 128.0).range(0.0, 7.0).more().doc(spread_doc),
    ]
}

/// The `level` param, -12 dB by default.
pub fn level_param(doc: &str) -> ParamSpec {
    ParamSpec::float("level", -12.0)
        .clamp(MIN_LEVEL_DB, MAX_LEVEL_DB)
        .range(-60.0, 0.0)
        .unit(Unit::Decibels)
        .doc(doc)
}

/// A time param in seconds on a log slider.
pub fn seconds_param(name: &str, default: f64, doc: &str) -> ParamSpec {
    ParamSpec::float(name, default)
        .clamp(0.0, 60.0)
        .range(0.005, 20.0)
        .curve(Curve::Log)
        .unit(Unit::Seconds)
        .doc(doc)
}

/// The tuning the pitch params give.
pub fn tuning(cx: &CookContext<'_>) -> Result<Tuning, CookError> {
    Ok(Tuning::new(
        cx.text("key")?,
        cx.int("octave")?,
        cx.text("scale")?,
        cx.float("note")?,
        cx.float("range")?,
        cx.int("spread")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(prev: Option<Seen>, samples: &[f64], rate: Option<f64>, now: f64, dir: Direction) -> Vec<f64> {
        let mut out = Vec::new();
        crossings(prev, samples, rate, now, 0.0, dir, &mut out);
        out.iter().map(|c| c.time).collect()
    }

    #[test]
    fn a_pulse_crosses_at_the_previous_cook() {
        let prev = Some(Seen { time: 1.0, value: 0.0 });
        assert_eq!(run(prev, &[1.0], Some(60.0), 1.0 + 1.0 / 60.0, Direction::Up), [1.0]);
        assert!(run(prev, &[1.0], Some(60.0), 1.1, Direction::Down).is_empty());
        // Back down to 0 crosses downwards, at the 0.
        let high = Some(Seen { time: 1.0, value: 1.0 });
        assert!(run(high, &[0.0], Some(60.0), 1.1, Direction::Up).is_empty());
        assert_eq!(run(high, &[0.0], Some(60.0), 1.1, Direction::Down), [1.1]);
        // A wave through 0 crosses once each way, also when a sample lands on 0.
        let mut out = Vec::new();
        let mut seen = Some(Seen { time: 0.0, value: -1.0 });
        for (k, v) in [0.0, 1.0, 0.0, -1.0, 0.0, 1.0].into_iter().enumerate() {
            seen = crossings(seen, &[v], Some(1.0), (k + 1) as f64, 0.0, Direction::Both, &mut out);
        }
        let got: Vec<(f64, bool)> = out.iter().map(|c| (c.time, c.rising)).collect();
        assert_eq!(got, [(1.0, true), (3.0, false), (5.0, true)]);
        // No history: nothing.
        assert!(run(None, &[1.0], Some(60.0), 1.1, Direction::Both).is_empty());
    }

    #[test]
    fn a_crossing_is_interpolated_between_samples() {
        let prev = Some(Seen { time: 0.0, value: -1.0 });
        let t = run(prev, &[3.0], Some(10.0), 0.4, Direction::Both);
        assert_eq!(t.len(), 1);
        assert!((t[0] - 0.1).abs() < 1e-12);
    }

    /// Several samples per cook: each newer than the last cook counts, older ones were seen.
    #[test]
    fn every_new_sample_counts_old_ones_are_skipped() {
        // Samples at 0.6, 0.7, 0.8, 0.9, 1.0 (rate 10, now 1.0); the last cook was at 0.75.
        let prev = Some(Seen { time: 0.75, value: 1.0 });
        let s = [-5.0, 5.0, -1.0, 1.0, -1.0];
        let t = run(prev, &s, Some(10.0), 1.0, Direction::Both);
        // 0.75 (1) -> 0.8 (-1): 0.775; 0.8 -> 0.9: 0.85; 0.9 -> 1.0: 0.95.
        assert_eq!(t.len(), 3, "{t:?}");
        for (a, b) in t.iter().zip([0.775, 0.85, 0.95]) {
            assert!((a - b).abs() < 1e-9, "{t:?}");
        }
        // An index axis: only the last value, at now.
        let t = run(Some(Seen { time: 0.9, value: 1.0 }), &s, None, 1.0, Direction::Down);
        assert_eq!(t, [0.95]);
    }
}
