//! Which note a lane of `audio.instrument` or `audio.ambience` plays: a key, a scale, an octave and a position
//! 0..1 within a range of octaves, turned into a note number and a frequency.
//!
//! Note numbers follow MIDI: 60 is middle C (C4), 69 the A above it at 440 Hz, equal
//! temperament. With a scale, `note` is rounded to the nearest degree of the scale; lane `i`
//! adds `i * spread` degrees. With `off`, `note` maps continuously and lane `i` adds
//! `i * spread` semitones.

/// The keys, in the order of the `key` choice: C is 0, B is 11.
pub const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// The scales, in the order of the `scale` choice.
pub const SCALES: [&str; 6] = ["major_pentatonic", "minor_pentatonic", "major", "minor", "chromatic", "off"];

/// Highest and lowest note number computed; far beyond hearing either way, so the frequency
/// stays finite whatever the params.
const NOTE_LIMIT: f64 = 400.0;

/// The semitones above the root of each degree of `scale`; `None` for `off` (free pitch).
pub fn steps(scale: &str) -> Option<&'static [u8]> {
    match scale {
        "minor_pentatonic" => Some(&[0, 3, 5, 7, 10]),
        "major" => Some(&[0, 2, 4, 5, 7, 9, 11]),
        "minor" => Some(&[0, 2, 3, 5, 7, 8, 10]),
        "chromatic" => Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
        "off" => None,
        _ => Some(&[0, 2, 4, 7, 9]),
    }
}

/// The index of `key` in [`KEYS`] (0 for C, also for an unknown name).
pub fn key_index(key: &str) -> usize {
    KEYS.iter().position(|k| *k == key).unwrap_or(0)
}

/// Where the notes are, for every lane of one cook.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// The note number `note` 0 plays.
    pub root: f64,
    /// The degrees of the scale; `None` for free pitch.
    pub steps: Option<&'static [u8]>,
    /// `note` 0..1, as given.
    pub note: f64,
    /// Octaves `note` 0..1 spans.
    pub range: f64,
    /// Degrees (semitones for free pitch) added per lane.
    pub spread: i64,
}

impl Tuning {
    pub fn new(key: &str, octave: i64, scale: &str, note: f64, range: f64, spread: i64) -> Self {
        let octave = octave.clamp(-10, 20) as f64;
        Self {
            root: 12.0 * (octave + 1.0) + key_index(key) as f64,
            steps: steps(scale),
            note: if note.is_finite() { note } else { 0.0 },
            range: if range.is_finite() { range } else { 0.0 },
            spread,
        }
    }

    /// The note number of lane `lane`: whole with a scale, fractional with free pitch.
    pub fn note_number(&self, lane: usize) -> f64 {
        let lane = i64::try_from(lane).unwrap_or(i64::MAX);
        let n = match self.steps {
            None => {
                let shift = lane.saturating_mul(self.spread) as f64;
                self.root + self.note * self.range * 12.0 + shift
            }
            Some(steps) => {
                let per_octave = steps.len().max(1) as i64;
                let base = (self.note * self.range * per_octave as f64).round().clamp(-1e6, 1e6) as i64;
                let degree = base.saturating_add(lane.saturating_mul(self.spread));
                let octave = degree.div_euclid(per_octave);
                let step = steps.get(degree.rem_euclid(per_octave) as usize).copied().unwrap_or(0);
                self.root + 12.0 * octave.clamp(-1_000_000, 1_000_000) as f64 + f64::from(step)
            }
        };
        n.clamp(-NOTE_LIMIT, NOTE_LIMIT)
    }

    /// The frequency of lane `lane` in Hz.
    pub fn hertz(&self, lane: usize) -> f64 {
        hertz(self.note_number(lane))
    }
}

/// The frequency of note number `n` in equal temperament, A4 (69) at 440 Hz.
pub fn hertz(n: f64) -> f64 {
    440.0 * ((n.clamp(-NOTE_LIMIT, NOTE_LIMIT) - 69.0) / 12.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(t: &Tuning, lanes: usize) -> Vec<String> {
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        (0..lanes)
            .map(|i| {
                let n = t.note_number(i).round() as i64;
                format!("{}{}", NAMES[n.rem_euclid(12) as usize], n.div_euclid(12) - 1)
            })
            .collect()
    }

    #[test]
    fn the_benchmark_rings_play_f_major_pentatonic_from_f3() {
        let t = Tuning::new("F", 3, "major_pentatonic", 0.0, 2.0, 1);
        assert_eq!(names(&t, 10), ["F3", "G3", "A3", "C4", "D4", "F4", "G4", "A4", "C5", "D5"]);
        let expect = [174.61, 196.00, 220.00, 261.63, 293.66, 349.23, 392.00, 440.00, 523.25, 587.33];
        for (i, e) in expect.iter().enumerate() {
            assert!((t.hertz(i) - e).abs() < 0.01, "lane {i}: {}", t.hertz(i));
        }
    }

    #[test]
    fn a4_is_440_and_c4_is_middle_c() {
        assert_eq!(hertz(69.0), 440.0);
        let t = Tuning::new("C", 4, "major", 0.0, 2.0, 1);
        assert_eq!(t.note_number(0), 60.0);
        assert!((t.hertz(0) - 261.6256).abs() < 1e-3);
        let a = Tuning::new("A", 4, "chromatic", 0.0, 2.0, 1);
        assert_eq!(a.hertz(0), 440.0);
    }

    #[test]
    fn note_rounds_to_the_nearest_degree_over_the_range() {
        // C major pentatonic over 2 octaves: 10 degrees from note 0 to note 1.
        let at = |note: f64| Tuning::new("C", 4, "major_pentatonic", note, 2.0, 1).note_number(0);
        assert_eq!(at(0.0), 60.0);
        assert_eq!(at(1.0), 84.0);
        assert_eq!(at(0.5), 72.0);
        // 0.14 * 10 = 1.4: degree 1 (D), 0.16 * 10 = 1.6: degree 2 (E).
        assert_eq!(at(0.14), 62.0);
        assert_eq!(at(0.16), 64.0);
        // Minor and major scales.
        let minor = Tuning::new("A", 3, "minor", 0.0, 1.0, 1);
        assert_eq!(
            (0..8).map(|i| minor.note_number(i)).collect::<Vec<_>>(),
            [57., 59., 60., 62., 64., 65., 67., 69.]
        );
        let mp = Tuning::new("E", 2, "minor_pentatonic", 0.0, 1.0, 1);
        assert_eq!((0..6).map(|i| mp.note_number(i)).collect::<Vec<_>>(), [40., 43., 45., 47., 50., 52.]);
    }

    #[test]
    fn chromatic_steps_by_semitones_and_off_is_continuous() {
        let c = Tuning::new("C", 4, "chromatic", 0.5, 1.0, 1);
        assert_eq!(c.note_number(0), 66.0);
        assert_eq!(c.note_number(3), 69.0);
        let off = |note: f64| Tuning::new("C", 4, "off", note, 2.0, 1).note_number(0);
        assert!((off(0.125) - 63.0).abs() < 1e-9);
        assert!((off(0.13) - 63.12).abs() < 1e-9);
        // Free pitch: lanes step by semitones.
        assert!((Tuning::new("C", 4, "off", 0.0, 2.0, 2).note_number(3) - 66.0).abs() < 1e-9);
    }

    #[test]
    fn octave_and_key_shift_the_root() {
        assert_eq!(Tuning::new("C", 5, "major", 0.0, 2.0, 1).note_number(0), 72.0);
        assert_eq!(Tuning::new("C", -1, "major", 0.0, 2.0, 1).note_number(0), 0.0);
        assert_eq!(Tuning::new("B", 4, "major", 0.0, 2.0, 1).note_number(0), 71.0);
        assert_eq!(Tuning::new("C#", 4, "major", 0.0, 2.0, 1).note_number(1), 63.0);
        // A spread of 0 puts every lane on one note, a negative one walks down.
        let t = Tuning::new("C", 4, "major", 0.0, 2.0, 0);
        assert_eq!(t.note_number(5), 60.0);
        assert_eq!(Tuning::new("C", 4, "major", 0.0, 2.0, -1).note_number(1), 59.0);
        assert_eq!(key_index("H"), 0);
    }

    #[test]
    fn extremes_stay_finite() {
        for note in [f64::NAN, f64::INFINITY, -1e300, 1e300, -5.0, 5.0] {
            for spread in [i64::MIN, -1000, 0, 1000, i64::MAX] {
                for scale in SCALES {
                    let t = Tuning::new("G#", i64::MAX, scale, note, 1e300, spread);
                    for lane in [0, 1, 127, usize::MAX] {
                        let f = t.hertz(lane);
                        assert!(f.is_finite() && f > 0.0, "{note} {spread} {scale} {lane}: {f}");
                    }
                }
            }
        }
    }
}
