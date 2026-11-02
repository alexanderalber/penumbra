//! A cue list for `chan.sequence` from the sections of a sound file: one step per section or
//! per track, timed to the boundaries, with a cross-fade through each blend zone.
//!
//! The steps are written for a show whose `clock` is the file's time (`seconds` of a
//! `chan.beat_grid`), so step k plays while the playhead is in section k, through seeks and
//! loops. Times are rounded to whole milliseconds first and the steps' seconds are differences
//! of those, so the rounding never adds up along a long file.

use penumbra_audio::beats::Sections;
use penumbra_core::param::MAX_TEXT;

use super::beat_grid::letter;

/// What one step stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Section,
    Track,
}

/// What names a step's piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pieces {
    /// The kind of sound (`section_label`): `kind0`, `kind1`, ...
    Label,
    /// The letter within the track (`section_letter`): `A`, `B`, ...
    Letter,
    /// The track (`track_index`): `track0`, `track1`, ...
    Track,
}

/// How one step leads into the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// A cross-fade through the blend zone where the music has one, a cut elsewhere.
    Zone,
    /// A cut at every boundary.
    Cut,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "section" => Some(Level::Section),
            "track" => Some(Level::Track),
            _ => None,
        }
    }
}

impl Pieces {
    pub fn parse(s: &str) -> Option<Pieces> {
        match s {
            "label" => Some(Pieces::Label),
            "letter" => Some(Pieces::Letter),
            "track" => Some(Pieces::Track),
            _ => None,
        }
    }

    /// The piece named by number `n`.
    fn name(self, n: usize) -> String {
        match self {
            Pieces::Label => format!("kind{n}"),
            Pieces::Letter => letter(n),
            Pieces::Track => format!("track{n}"),
        }
    }
}

impl Blend {
    pub fn parse(s: &str) -> Option<Blend> {
        match s {
            "zone" => Some(Blend::Zone),
            "cut" => Some(Blend::Cut),
            _ => None,
        }
    }
}

/// A cue list ready for a `chan.sequence`'s `steps` and `names`.
#[derive(Clone, Debug, PartialEq)]
pub struct Cues {
    pub steps: String,
    /// Every piece from number 0 to the highest used, so piece k is `in<k>` of a `tex.sequence`
    /// and the `piece` channel equals the section's number.
    pub names: String,
    /// One per step: when it begins (s into the file) and its piece number.
    pub starts: Vec<(f64, usize)>,
    /// Where the last step ends, s into the file.
    pub end: f64,
}

/// Shortest step written, in milliseconds (`chan.sequence` lengthens shorter ones).
const MIN_STEP_MS: i64 = 100;
/// Shortest fade written, in milliseconds; a shorter blend is a cut.
const MIN_FADE_MS: i64 = 50;

/// Seconds as whole milliseconds; anything that is no number counts as 0.
fn ms(t: f64) -> i64 {
    if t.is_finite() {
        // `as`: clamped far inside i64.
        (t * 1000.0).round().clamp(0.0, 1e15) as i64
    } else {
        0
    }
}

/// One step before it is written: where it begins, the fade into it, its piece and its comment.
struct Unit {
    start: i64,
    fade: i64,
    piece: usize,
    note: String,
}

/// The cue list of `sections`, the last step ending at `end` (s into the file: the file's
/// length, or the sections' end when that is later). `source` names the file in the first line.
pub fn cues(
    sections: &Sections,
    end: f64,
    source: &str,
    level: Level,
    pieces: Pieces,
    blend: Blend,
) -> Result<Cues, String> {
    let list = sections.list();
    if list.is_empty() {
        return Err("the file has no sections".into());
    }
    let end = ms(if end.is_finite() { end.max(sections.end()) } else { sections.end() });
    let mut units: Vec<Unit> = Vec::new();
    for (i, s) in list.iter().enumerate() {
        if level == Level::Track && i > 0 && !s.track_start {
            continue;
        }
        let piece = match pieces {
            Pieces::Label => s.label,
            Pieces::Letter => s.letter,
            Pieces::Track => s.track,
        };
        let (z0, z1) = (ms(s.zone.0), ms(s.zone.1));
        let fading = blend == Blend::Zone && i > 0 && z1 - z0 >= MIN_FADE_MS;
        // The first step starts with the file, like the section channels before the first bar.
        let start = if i == 0 {
            0
        } else if fading {
            z1
        } else {
            ms(s.start)
        };
        let note = match level {
            Level::Section => format!(
                "section {i}: {}, kind {}, track {}, from {:.2} s",
                letter(s.letter),
                s.label,
                s.track,
                s.start
            ),
            Level::Track => format!("track {}, from {:.2} s", s.track, s.start),
        };
        // A boundary too close to the one before (or past the end) is left out: the step before
        // runs on through it.
        let room = units.last().is_none_or(|u| start >= u.start + MIN_STEP_MS) && start + MIN_STEP_MS <= end;
        if room || units.is_empty() {
            units.push(Unit { start, fade: if fading { z1 - z0 } else { 0 }, piece, note });
        }
    }
    let highest = units.iter().map(|u| u.piece).max().unwrap_or(0);
    let names = (0..=highest).map(|n| pieces.name(n)).collect::<Vec<_>>().join(" ");
    let what = match level {
        Level::Section => "section",
        Level::Track => "track",
    };
    let write = |notes: bool| -> String {
        let mut text = String::new();
        if notes {
            text.push_str(&format!(
                "# One step per {what} of {source}, written by beats.cues. Seconds follow the file:\n\
                 # bind clock to the seconds of its chan.beat_grid.\n"
            ));
        }
        for (k, u) in units.iter().enumerate() {
            let next = units.get(k + 1);
            let until = next.map_or(end, |n| n.start).max(u.start + MIN_STEP_MS);
            let seconds = (until - u.start) as f64 / 1000.0;
            let fade = next.map_or(0, |n| n.fade.min(until - u.start));
            let name = pieces.name(u.piece);
            let mut line = if fade > 0 {
                format!("{name} {seconds:.3} fade {:.3}", fade as f64 / 1000.0)
            } else {
                format!("{name} {seconds:.3}")
            };
            if notes {
                line = format!("{line:<28} # {}", u.note);
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    };
    let mut steps = write(true);
    if steps.len() > MAX_TEXT {
        steps = write(false);
    }
    if steps.len() > MAX_TEXT {
        return Err(format!(
            "{} steps do not fit into a steps param ({MAX_TEXT} bytes); try level track",
            units.len()
        ));
    }
    let starts = units.iter().map(|u| (u.start as f64 / 1000.0, u.piece)).collect();
    Ok(Cues { steps, names, starts, end: end as f64 / 1000.0 })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use penumbra_audio::beats::{Beats, ENVELOPE_RATE, envelopes_of};

    use super::*;
    use crate::audio::beat_grid::{Grid, Structure, find_sections};
    use crate::sequence::Show;

    /// 120 beats per minute, a bar every 2 s, 96 s: a low hum with a kick, a high tone with
    /// hiss from 32 s, the hum again from 64 s after an 8 s cross-fade.
    fn structure() -> (Structure, f64) {
        let rate = ENVELOPE_RATE;
        let seconds = 96.0;
        let n = (seconds * rate) as usize;
        let mut seed = 3u32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                use std::f64::consts::TAU;
                let t = i as f64 / rate;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = f64::from(seed >> 8) / f64::from(1u32 << 23) - 1.0;
                let beat = t % 0.5;
                let low =
                    0.6 * (-beat / 0.07).exp() * (TAU * 55.0 * beat).sin() + 0.2 * (TAU * 82.0 * t).sin();
                let high = 0.15 * (TAU * 3000.0 * t).sin() + 0.2 * noise * (-beat / 0.05).exp();
                let w = if t < 32.0 {
                    0.0
                } else if t < 60.0 {
                    1.0
                } else {
                    (1.0 - (t - 60.0) / 8.0).clamp(0.0, 1.0)
                };
                ((1.0 - w) * low + w * high) as f32
            })
            .collect();
        let env = envelopes_of(&x, 1, rate).unwrap();
        let beats = Beats {
            beats: (0..192).map(|i| f64::from(i) * 0.5).collect(),
            downbeats: (0..48).map(|i| f64::from(i) * 2.0).collect(),
            downbeat_logits: Vec::new(),
        };
        let grid = Arc::new(Grid::new(beats, f64::INFINITY));
        (find_sections(&grid, &Arc::new(env)).unwrap(), seconds)
    }

    /// The written steps, comments left out.
    fn lines(c: &Cues) -> Vec<&str> {
        c.steps.lines().map(|l| l.split('#').next().unwrap().trim()).filter(|l| !l.is_empty()).collect()
    }

    /// Inside section i, away from both blends.
    fn inside(list: &[penumbra_audio::beats::Section], i: usize) -> f64 {
        let a = list[i].zone.1.max(list[i].start);
        let b = list.get(i + 1).map_or(list[i].end, |n| n.zone.0.min(n.start));
        (a + b) / 2.0
    }

    #[test]
    fn a_step_per_section_plays_while_the_file_is_in_that_section() {
        let (s, seconds) = structure();
        let list = s.sections.list();
        assert!(list.len() >= 3, "{list:?}");
        let c = cues(&s.sections, seconds, "synth.wav", Level::Section, Pieces::Label, Blend::Zone).unwrap();
        assert_eq!(lines(&c).len(), list.len(), "{}", c.steps);
        assert!(c.steps.starts_with("# One step per section of synth.wav"));
        let kinds = s.sections.kinds();
        let names: Vec<String> = (0..kinds).map(|k| format!("kind{k}")).collect();
        assert_eq!(c.names, names.join(" "));
        let show = Show::new(&c.steps, &c.names, 0.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        // The show is as long as the file: the steps' rounded seconds add up exactly.
        assert!((show.length() - seconds).abs() < 1e-9, "{}", show.length());
        assert!((c.end - seconds).abs() < 1e-9);
        for (i, sec) in list.iter().enumerate() {
            let m = show.at(inside(list, i));
            assert_eq!(m.step, i as i64);
            assert_eq!(m.piece, Some(sec.label));
            assert_eq!(m.mix, 0.0);
        }
        // The hum comes back: the first and the last section share a kind and so a piece.
        assert_eq!(list[0].label, list[list.len() - 1].label);
        // The blend into the last section is a zone, and the show fades through it.
        let last = list.len() - 1;
        let (z0, z1) = list[last].zone;
        assert!(z1 - z0 > 1.0, "{:?}", list[last].zone);
        let m = show.at((z0 + z1) / 2.0);
        assert!(m.mix > 0.3 && m.mix < 0.7, "{m:?}");
        assert!(show.at(z0 - 0.01).mix == 0.0 && show.at(z1).step == last as i64);
        // After the end: black.
        assert_eq!(show.at(seconds + 1.0).step, list.len() as i64);
    }

    #[test]
    fn steps_per_track_by_letter_and_cuts() {
        let (s, seconds) = structure();
        let list = s.sections.list();
        let c = cues(&s.sections, seconds, "synth.wav", Level::Section, Pieces::Letter, Blend::Cut).unwrap();
        assert!(!c.steps.contains("fade"), "{}", c.steps);
        assert!(lines(&c).iter().all(|l| l.split_whitespace().count() == 2));
        let show = Show::new(&c.steps, &c.names, 0.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        assert!(c.names.starts_with('A'));
        for (i, sec) in list.iter().enumerate() {
            // A cut at the section's start, wherever its zone is.
            assert_eq!(show.at(sec.start + 0.01).step, i as i64);
            assert_eq!(show.at(sec.start + 0.01).piece, Some(sec.letter));
        }
        let c = cues(&s.sections, seconds, "synth.wav", Level::Track, Pieces::Track, Blend::Zone).unwrap();
        assert_eq!(lines(&c).len(), s.sections.tracks(), "{}", c.steps);
        assert!(c.steps.starts_with("# One step per track"));
        let show = Show::new(&c.steps, &c.names, 0.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        assert!((show.length() - seconds).abs() < 1e-9);
    }

    #[test]
    fn hostile_lengths_give_steps_that_still_play() {
        let (s, seconds) = structure();
        // An end before the sections' end, or no number: the sections' end.
        for end in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let c = cues(&s.sections, end, "x", Level::Section, Pieces::Label, Blend::Zone).unwrap();
            let show = Show::new(&c.steps, &c.names, 0.0);
            assert!(show.problems().is_empty(), "{end}: {:?}", show.problems());
            assert!(show.length() >= s.sections.end() - 1e-3 && show.length() <= seconds + 1e-3, "{end}");
        }
        assert_eq!(Level::parse("bar"), None);
        assert_eq!(Pieces::parse(""), None);
        assert_eq!(Blend::parse("fade"), None);
    }
}
