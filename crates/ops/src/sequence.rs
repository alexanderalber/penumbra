//! `chan.sequence`: a show of pieces played one after another, with cuts, cross-fades, dips to
//! black and flashes between them, from a list of steps.
//!
//! The pieces, black and white form a row of entries; the show's time is a place in that row
//! that moves on by one at each cut and gradually during a fade. From the place come the two
//! entries shown and the share of the second (`from`, `to`, `mix`), which a `tex.sequence` per
//! output turns into the picture, and a run gate per piece for the pieces' own clocks. Nothing
//! but the show's clock is state: the same seconds always give the same answer.
//!
//! A step may hold (`hold` instead of seconds): the show's clock stops just before the step's
//! way out until `go` rises, as a cue list waits for GO. `go` also cuts a timed step short.
//!
//! A `clock` bound to a channel replaces the show's own clock: the show stands at those seconds,
//! whatever came before, so it follows a sound file's playhead (`seconds` of a
//! `chan.beat_grid`) through seeks and loops. Holds, `go` and the restart do nothing then.
//!
//! In show mode the place in the show (seconds, the holds already let go, the rounds) is kept
//! across a crash restart ([`Operator::show_place`]): the show goes on where it was instead of
//! starting over at its first cue. The place names the show it belongs to, so a show edited
//! meanwhile starts over as usual.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Tick, Unit};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

/// The kind of the show sequencer.
pub const SEQUENCE: &str = "chan.sequence";
/// `from` and `to` while black is shown.
pub const BLACK: f64 = -1.0;
/// `from` and `to` while white is shown (a flash).
pub const WHITE: f64 = -2.0;
/// Largest steps file read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Shortest step, in seconds.
const MIN_DURATION: f64 = 0.1;
/// Shortest fade, dip or flash, in seconds.
const MIN_LENGTH: f64 = 0.05;
/// A share of the second entry below this counts as none (the same threshold as `tex.switch`).
const MIX_EPSILON: f64 = 1e-6;
/// `clock` while the show runs on its own clock.
const OWN_CLOCK: f64 = -1.0;
/// How long before its way out a holding step stops the clock, in seconds: a cut at the end of
/// the step must not have happened yet.
const HOLD_BEFORE: f64 = 1e-3;

/// Registers `chan.sequence`.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            SEQUENCE,
            "A timed show: pieces one after another with cuts, fades, dips to black and flashes. Drives \
             the tex.sequence that shows it, and a run channel per piece",
        )
        .stateful()
        .param(ParamSpec::text("steps", "").multiline().doc(
            "One step per line: piece, seconds, transition into the next step, its length. E.g. \
             `faces 10 fade 2`. Seconds `hold`: the step stays until `go`, then its transition plays. \
             Transitions: cut, fade (cross-fade over the step's last seconds), \
             black (fade out to black, the next cuts in), flash (cut to white, which fades into the next \
             step over its first seconds). # starts a comment. The last step's transition leads into \
             the black the show ends in",
        ))
        .param(ParamSpec::file("file", &["json", "txt"]).doc(
            "Read the steps from this file instead (relative to the project directory; saving it \
             reloads): lines as in `steps`, or JSON {\"lead_in\": s, \"steps\": [{\"piece\", \"duration\" \
             (seconds or \"hold\"), \"transition\", \"length\"}]}, whose lead_in then wins over the param",
        ))
        .param(ParamSpec::text("names", "").doc(
            "The pieces in input order, separated by spaces: piece k is input in<k> of a tex.sequence \
             and has a channel run_<name>. Steps naming another piece are left out. Empty: the pieces \
             in the order the steps first name them",
        ))
        .param(
            ParamSpec::float("lead_in", 1.0)
                .clamp(0.0, 1e6)
                .range(0.0, 10.0)
                .unit(Unit::Seconds)
                .doc("Black before the first step, so its piece starts over when the show starts"),
        )
        .param(ParamSpec::float("run", 1.0).range(0.0, 1.0).doc(
            "On from 0.5 up (with `equals` set: while it equals that number). Each time it turns on the \
             show starts from its beginning; while it is off it shows black and every run gate is 0",
        ))
        .param(ParamSpec::int("equals", -1).clamp(-1.0, 1e9).range(-1.0, 20.0).special(-1.0, "off").doc(
            "When set, `run` is on while it equals this number: bind `run` to the channel that says \
             which piece plays and set this to the show's own number. While off, `run` is on from 0.5 up",
        ))
        .param(ParamSpec::float("go", 0.0).range(0.0, 1.0).doc(
            "Each time it rises past 0.5 the step on screen moves on: a holding step starts its \
             transition, a timed one cuts its time short. Ignored while a transition plays. Bind it \
             to a key's gate, or to an OSC address listed under `triggers` on a chan.osc_in",
        ))
        .param(
            ParamSpec::float("clock", OWN_CLOCK)
                .clamp(OWN_CLOCK, MAX_PLACE_SECONDS)
                .range(OWN_CLOCK, 600.0)
                .unit(Unit::Seconds)
                .special(OWN_CLOCK, "own")
                .doc(
                    "The show's seconds, set from outside. Bind it to `seconds` of a chan.beat_grid, and the \
                     show stands where the music stands, also after a seek or a loop. `hold`, `go` and the \
                     restart when `run` turns on do nothing then. beats.cues writes steps for this and binds \
                     it. -1 (own): the show runs on its own clock from the moment `run` turns on",
                ),
        )
        .param(
            ParamSpec::choice("mode", &["once", "loop"])
                .doc("What the show does after its last step")
                .choice_doc("once", "it ends in black and stays there")
                .choice_doc("loop", "it starts over from its lead-in, round after round"),
        )
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`seconds` into the show (loop: into the round), `step` (counted from 0; -1 in the lead-in, \
             the number of steps after the last), `piece` (the step's piece number, -1 for none), \
             `from`, `to` and `mix` (the entries shown: a piece number, -1 black, -2 white; mix is the \
             share of `to`, 0 while one entry shows), `done` (1 once the show has played through), \
             `length` (the show's seconds) and `run_<name>` per piece: 1 while it is shown, also \
             during its fades",
        )
        .time_dependent()
        .eager(),
        || Box::new(Sequence::default()) as Box<dyn Operator>,
    ))
}

/// What a show shows: a piece by its number, black or white.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Black,
    White,
    Piece(usize),
}

impl Entry {
    /// The entry's number on the `from` and `to` channels.
    pub fn code(self) -> f64 {
        match self {
            Entry::Black => BLACK,
            Entry::White => WHITE,
            Entry::Piece(k) => k as f64,
        }
    }

    /// The entry a `from` or `to` value names (rounded; anything not a piece number is black,
    /// a value near -2 white).
    pub fn from_code(x: f64) -> Entry {
        if !x.is_finite() {
            return Entry::Black;
        }
        let r = x.round();
        if r == WHITE {
            Entry::White
        } else if r < 0.0 || r > 1e9 {
            Entry::Black
        } else {
            Entry::Piece(r as usize)
        }
    }
}

/// How a step leads into the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transition {
    Cut,
    Fade,
    Black,
    Flash,
}

impl Transition {
    fn parse(name: &str) -> Option<Transition> {
        match name {
            "cut" => Some(Transition::Cut),
            "fade" => Some(Transition::Fade),
            "black" => Some(Transition::Black),
            "flash" => Some(Transition::Flash),
            _ => None,
        }
    }

    /// The length a step gets when it names none.
    fn default_length(self) -> f64 {
        match self {
            Transition::Cut => 0.0,
            Transition::Fade | Transition::Black => 1.5,
            Transition::Flash => 0.6,
        }
    }
}

/// A step as written, before its piece is looked up.
struct Written {
    piece: String,
    duration: Option<f64>,
    /// Written `hold` instead of seconds: the step waits for `go`.
    hold: bool,
    transition: Option<String>,
    length: Option<f64>,
    /// Where it was written, for messages ("line 3", "step 2").
    at: String,
}

/// The steps of a text: JSON when it starts with `{`, else one step per line. Also the lead-in
/// a JSON text names, and what was wrong with it.
fn parse(text: &str) -> (Option<f64>, Vec<Written>, Vec<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim_start().starts_with('{') { parse_json(text) } else { parse_lines(text) }
}

fn parse_lines(text: &str) -> (Option<f64>, Vec<Written>, Vec<String>) {
    let mut steps = Vec::new();
    let mut problems = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("");
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(piece) = words.first() else { continue };
        let at = format!("line {}", i + 1);
        let number = |k: usize| -> Result<Option<f64>, String> {
            match words.get(k) {
                None => Ok(None),
                Some(w) => match w.parse::<f64>() {
                    Ok(x) if x.is_finite() => Ok(Some(x)),
                    _ => Err(format!("{at}: `{w}` is no number; step left out")),
                },
            }
        };
        let hold = words.get(1).is_some_and(|w| w.eq_ignore_ascii_case("hold"));
        let duration = if hold { Ok(None) } else { number(1) };
        let (duration, length) = match (duration, number(3)) {
            (Ok(d), Ok(l)) => (d, l),
            (Err(e), _) | (_, Err(e)) => {
                problems.push(e);
                continue;
            }
        };
        if words.len() > 4 {
            problems
                .push(format!("{at}: more than piece, seconds, transition and length; the rest is ignored"));
        }
        steps.push(Written {
            piece: (*piece).to_string(),
            duration,
            hold,
            transition: words.get(2).map(|w| (*w).to_string()),
            length,
            at,
        });
    }
    (None, steps, problems)
}

fn parse_json(text: &str) -> (Option<f64>, Vec<Written>, Vec<String>) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (None, Vec::new(), vec![format!("not valid JSON: {e}")]),
    };
    let mut problems = Vec::new();
    let lead = value.get("lead_in").and_then(serde_json::Value::as_f64).filter(|x| x.is_finite());
    let Some(list) = value.get("steps").and_then(serde_json::Value::as_array) else {
        return (lead, Vec::new(), vec!["the JSON has no list `steps`".into()]);
    };
    let mut steps = Vec::new();
    for (i, st) in list.iter().enumerate() {
        let at = format!("step {}", i + 1);
        let Some(piece) = st.get("piece").and_then(serde_json::Value::as_str) else {
            problems.push(format!("{at}: no `piece`; step left out"));
            continue;
        };
        let number = |key: &str| st.get(key).and_then(serde_json::Value::as_f64).filter(|x| x.is_finite());
        let hold = st
            .get("duration")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|d| d.eq_ignore_ascii_case("hold"));
        steps.push(Written {
            piece: piece.to_string(),
            duration: number("duration"),
            hold,
            transition: st.get("transition").and_then(serde_json::Value::as_str).map(str::to_string),
            length: number("length"),
            at,
        });
    }
    (lead, steps, problems)
}

/// A step ready to play.
#[derive(Clone, Copy, Debug)]
struct Step {
    piece: usize,
    duration: f64,
    hold: bool,
}

/// A show worked out: the row of entries and the moments the place in it moves on by one.
#[derive(Debug)]
pub struct Show {
    names: Vec<String>,
    steps: Vec<Step>,
    /// When each step starts, in seconds from the show's start, and after them the end of the
    /// last.
    starts: Vec<f64>,
    /// Per step, when its way out starts (`go` jumps there), and whether the clock holds just
    /// before it.
    exits: Vec<(f64, bool)>,
    entries: Vec<Entry>,
    /// Per move to the next entry: when it starts and how long it takes (0: a cut).
    moves: Vec<(f64, f64)>,
    /// Seconds from the start to the end of the last transition.
    length: f64,
    problems: Vec<String>,
}

/// What a show shows at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub step: i64,
    pub piece: Option<usize>,
    pub from: Entry,
    pub to: Entry,
    /// The share of `to`; 0 while one entry shows.
    pub mix: f64,
}

impl Moment {
    /// Black, no step: a show that is off.
    const OFF: Moment = Moment { step: -1, piece: None, from: Entry::Black, to: Entry::Black, mix: 0.0 };

    /// Whether piece `k` is shown.
    pub fn shows(&self, k: usize) -> bool {
        self.from == Entry::Piece(k) || (self.mix >= MIX_EPSILON && self.to == Entry::Piece(k))
    }
}

impl Show {
    /// The show of `text` (see [`parse`]) over the pieces `names` (separated by spaces; empty:
    /// as the steps name them), after `lead_in` seconds of black unless the text names its own.
    pub fn new(text: &str, names: &str, lead_in: f64) -> Show {
        let (lead, written, mut problems) = parse(text);
        let lead = lead.unwrap_or(lead_in).max(0.0);
        let fixed = !names.trim().is_empty();
        let mut known: Vec<String> = Vec::new();
        for n in names.split_whitespace() {
            if known.iter().any(|k| k == n) {
                problems.push(format!("`{n}` is in names twice"));
            } else {
                known.push(n.to_string());
            }
        }
        let mut steps = Vec::new();
        let mut leads = Vec::new();
        for w in written {
            let piece = match known.iter().position(|k| *k == w.piece) {
                Some(k) => k,
                None if fixed => {
                    problems.push(format!("{}: no piece `{}` in names; step left out", w.at, w.piece));
                    continue;
                }
                None => {
                    known.push(w.piece.clone());
                    known.len() - 1
                }
            };
            let kind = match w.transition.as_deref() {
                None => Transition::Cut,
                Some(t) => Transition::parse(t).unwrap_or_else(|| {
                    problems
                        .push(format!("{}: transition `{t}` is none of cut, fade, black, flash; cut", w.at));
                    Transition::Cut
                }),
            };
            let length = match kind {
                Transition::Cut => 0.0,
                _ => w.length.unwrap_or(kind.default_length()).max(MIN_LENGTH),
            };
            steps.push(Step { piece, duration: w.duration.unwrap_or(10.0).max(MIN_DURATION), hold: w.hold });
            leads.push((kind, length, w.at));
        }
        for n in &known {
            if Name::new(&format!("run_{n}")).is_err() {
                problems
                    .push(format!("`{n}` makes no channel run_{n} (letters, digits and _); no gate for it"));
            }
        }
        if steps.is_empty() {
            problems.push("no steps".into());
        }
        // The row: black first, then per step its piece, a black after a dip to black, a white
        // for a flash; black at the end.
        let mut entries = vec![Entry::Black];
        let mut moves = vec![(lead, 0.0)];
        let mut starts = Vec::with_capacity(steps.len() + 1);
        let mut exits = Vec::with_capacity(steps.len());
        let mut t = lead;
        let count = steps.len();
        for (i, (step, (kind, length, at))) in steps.iter_mut().zip(&leads).enumerate() {
            entries.push(Entry::Piece(step.piece));
            starts.push(t);
            let last = i + 1 == count;
            let flash_in = match i.checked_sub(1).and_then(|p| leads.get(p)) {
                Some((Transition::Flash, l, _)) => *l,
                _ => 0.0,
            };
            let own = if matches!(kind, Transition::Fade | Transition::Black) { *length } else { 0.0 };
            let taken = flash_in + own;
            if step.hold {
                // Long enough for the flash coming in and the way out, with a moment between.
                step.duration = taken + MIN_DURATION;
            } else if taken > step.duration {
                problems.push(format!(
                    "{at}: its {} s are less than its transitions take ({taken} s)",
                    step.duration
                ));
            }
            let end = t + step.duration;
            exits.push((end - own, step.hold));
            match kind {
                Transition::Cut => moves.push((end, 0.0)),
                Transition::Fade => moves.push((end - length, *length)),
                Transition::Black => {
                    moves.push((end - length, *length));
                    if !last {
                        entries.push(Entry::Black);
                        moves.push((end, 0.0));
                    }
                }
                Transition::Flash => {
                    entries.push(Entry::White);
                    moves.push((end, 0.0));
                    moves.push((end, *length));
                }
            }
            t = end;
        }
        starts.push(t);
        // Each step's piece came in with its own round; after the last, the black it ends in.
        entries.push(Entry::Black);
        let length = moves.iter().map(|(a, w)| a + w).fold(lead, f64::max);
        Show { names: known, steps, starts, exits, entries, moves, length, problems }
    }

    /// Seconds from the start to the end of the last transition.
    pub fn length(&self) -> f64 {
        self.length
    }

    /// The pieces, by number.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// What was wrong with the steps (each left out, cut or shortened as it says).
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Where the clock stops at `t`: just before the way out of the first holding step from step
    /// `free` on, once `t` has reached that moment.
    fn held(&self, t: f64, free: usize) -> Option<f64> {
        let (exit, _) = self.exits.iter().skip(free).find(|(_, hold)| *hold)?;
        let at = (exit - HOLD_BEFORE).max(0.0);
        (t >= at).then_some(at)
    }

    /// What `go` does at `t`: the time to go on from, and the first step that may hold again.
    /// `None` while a transition plays and after the last step.
    fn go(&self, t: f64) -> Option<(f64, usize)> {
        let first = self.starts.first().copied().unwrap_or(0.0);
        if t < first {
            return Some((first, 0));
        }
        let step = usize::try_from(self.at(t).step).ok()?;
        let (exit, _) = *self.exits.get(step)?;
        // A holding step waits just before its exit; later than that its transition plays.
        (t <= exit).then_some((t.max(exit - HOLD_BEFORE), step + 1))
    }

    /// What the show shows `t` seconds after its start.
    pub fn at(&self, t: f64) -> Moment {
        let t = if t.is_finite() { t } else { 0.0 };
        let place: f64 = self
            .moves
            .iter()
            .map(|&(start, width)| {
                if width <= 0.0 {
                    if t >= start { 1.0 } else { 0.0 }
                } else {
                    ((t - start) / width).clamp(0.0, 1.0)
                }
            })
            .sum();
        let last = self.entries.len().saturating_sub(1);
        let lower = (place.floor().max(0.0) as usize).min(last);
        let share = if lower < last { (place - lower as f64).clamp(0.0, 1.0) } else { 0.0 };
        let from = self.entries.get(lower).copied().unwrap_or(Entry::Black);
        let blend = share >= MIX_EPSILON;
        let to = if blend { self.entries.get(lower + 1).copied().unwrap_or(Entry::Black) } else { from };
        let first = self.starts.first().copied().unwrap_or(0.0);
        let step = if t < first {
            -1
        } else {
            // `starts` ends with the end of the last step: past it, the number of steps.
            let begun = self.starts.iter().filter(|s| **s <= t).count();
            begun.saturating_sub(1).min(self.steps.len()) as i64
        };
        let piece = usize::try_from(step).ok().and_then(|s| self.steps.get(s)).map(|s| s.piece);
        Moment { step, piece, from, to, mix: if blend { share } else { 0.0 } }
    }
}

/// What a show was worked out from, to tell when to work it out again.
struct Source {
    text: String,
    names: String,
    lead_in: f64,
}

impl Source {
    /// A hash of the source (FNV-1a, the same in every build), so a place kept before a restart
    /// is taken only by the same show.
    fn fingerprint(&self) -> String {
        let bytes = self
            .text
            .as_bytes()
            .iter()
            .chain([0u8].iter())
            .chain(self.names.as_bytes())
            .chain([0u8].iter())
            .copied()
            .chain(self.lead_in.to_bits().to_le_bytes())
            .collect::<Vec<u8>>();
        let hash = bytes
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        format!("{hash:016x}")
    }
}

/// Where a running show stands, as kept across a crash restart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Place {
    /// [`Source::fingerprint`] of the show.
    show: String,
    /// Seconds into the show (loop: into the round).
    seconds: f64,
    /// Steps before this one no longer hold.
    free: usize,
    /// A looping show has been round once.
    round: bool,
}

/// Longest show a kept place may name, in seconds (about 30 years).
const MAX_PLACE_SECONDS: f64 = 1e9;
/// Largest kept place read.
const MAX_PLACE_BYTES: usize = 4096;

/// `chan.sequence`: the show's clock (its seconds at a tick, running on from there), when it
/// last cooked, the show and its steps file.
#[derive(Default)]
struct Sequence {
    /// The tick the clock was last set at, and its seconds then; `None` while the show is off.
    anchor: Option<(Tick, f64)>,
    /// Steps before this one no longer hold: `go` let them go.
    free: usize,
    /// Whether `go` was up in the last cook, to see it rise.
    go_up: bool,
    /// Whether a looping show has been round once.
    round: bool,
    last: Option<Tick>,
    /// Seconds into the show at the last cook while it ran.
    seconds: Option<f64>,
    /// A place from before a restart, taken at the next cook if the show is the same and on.
    resume: Option<Place>,
    show: Option<(Source, Arc<Show>)>,
    slot: AsyncSlot<String>,
    watch: Option<FileWatch>,
}

impl Sequence {
    /// The steps text: the `steps` param, or the file's text once read.
    fn text<'a>(&'a mut self, cx: &'a CookContext<'_>) -> Result<&'a str, CookError> {
        self.slot.poll();
        let file = cx.text("file")?.trim();
        if file.is_empty() {
            self.watch = None;
            return cx.text("steps");
        }
        let path = cx.resolve_path(file)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            self.slot.start(cx.spawn(move |_job| {
                let name = path.display();
                let len = std::fs::metadata(&path).map_err(|e| format!("{name}: {e}"))?.len();
                if len > MAX_FILE_BYTES {
                    return Err(format!("{name}: {len} bytes, a steps file may have {MAX_FILE_BYTES}"));
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("{name}: {e}"))?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        self.slot.latest().map(String::as_str).ok_or(CookError::Pending)
    }
}

impl Operator for Sequence {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let names = cx.text("names")?;
        let lead_in = cx.float("lead_in")?;
        let text = self.text(cx)?.to_string();
        let same = self
            .show
            .as_ref()
            .is_some_and(|(s, _)| s.text == text && s.names == names && s.lead_in == lead_in);
        if !same {
            let show = Arc::new(Show::new(&text, names, lead_in));
            self.show = Some((Source { text, names: names.to_string(), lead_in }, show));
        }
        let Some(show) = self.show.as_ref().map(|(_, s)| s.clone()) else {
            return Err(CookError::failed("no show"));
        };
        if let Some(first) = show.problems().first() {
            let more = show.problems().len() - 1;
            cx.warn(if more > 0 { format!("{first} (and {more} more)") } else { first.clone() });
        }

        let now = cx.time().tick;
        let run = cx.float("run")?;
        let equals = cx.int("equals")?;
        let on = if equals >= 0 { (run - equals as f64).abs() <= 1e-6 } else { run >= 0.5 };
        // A seek back starts it over.
        let back = self.last.is_some_and(|last| now.0 < last.0);
        self.last = Some(now);
        let go = cx.float("go")? >= 0.5;
        let rose = go && !self.go_up;
        self.go_up = go;
        let length = show.length();
        let looping = cx.text("mode")? == "loop" && length > 0.0;
        let clock = cx.float("clock")?;
        let seconds = if clock.is_finite() && clock > OWN_CLOCK / 2.0 {
            // From outside: nothing of the show's own clock is kept, nor a place to resume.
            self.anchor = None;
            self.resume = None;
            self.free = 0;
            self.round = looping && clock >= length;
            let t = if looping && clock >= length { clock.rem_euclid(length) } else { clock };
            on.then_some(t.clamp(0.0, length))
        } else {
            self.own_clock(on, back, now, rose, &show, looping)
        };
        self.seconds = seconds;
        let seconds = seconds.unwrap_or(0.0);
        let done = on && (self.round || seconds >= length);
        let moment = if on { show.at(seconds) } else { Moment::OFF };

        let channel = |name: &str, v: f64| -> Result<Channel, CookError> {
            Ok(Channel { name: Name::new(name)?, samples: vec![v] })
        };
        let mut channels = vec![
            channel("seconds", seconds)?,
            channel("step", moment.step as f64)?,
            channel("piece", moment.piece.map_or(-1.0, |k| k as f64))?,
            channel("from", moment.from.code())?,
            channel("to", moment.to.code())?,
            channel("mix", moment.mix)?,
            channel("done", if done { 1.0 } else { 0.0 })?,
            channel("length", length)?,
        ];
        for (k, n) in show.names().iter().enumerate() {
            // A name that makes no channel was reported with the show's problems.
            if let Ok(gate) = channel(&format!("run_{n}"), if moment.shows(k) { 1.0 } else { 0.0 }) {
                channels.push(gate);
            }
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, Chans::new(cx.rate().as_f64(), channels)?)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        self.watch.as_ref().map(|_| self.slot.status())
    }

    fn show_place(&self) -> Option<String> {
        if let Some(waiting) = &self.resume {
            // Not taken yet (the steps file is still loading): keep it for the next restart.
            return serde_json::to_string(waiting).ok();
        }
        let ((_, base), seconds) = (self.anchor?, self.seconds?);
        let (source, _) = self.show.as_ref()?;
        // Whole seconds while the clock runs, so the place changes about once a second; never
        // before the last jump (a start, a hold, a `go`), and exact while a step holds.
        let place = Place {
            show: source.fingerprint(),
            seconds: seconds.floor().max(base.min(seconds)),
            free: self.free,
            round: self.round,
        };
        serde_json::to_string(&place).ok()
    }

    fn restore_show_place(&mut self, place: &str) -> Result<(), String> {
        if place.len() > MAX_PLACE_BYTES {
            return Err(format!("a place of {} bytes is not a cue list's", place.len()));
        }
        let p: Place = serde_json::from_str(place).map_err(|e| format!("not a cue list's place: {e}"))?;
        if !(p.seconds.is_finite() && (0.0..=MAX_PLACE_SECONDS).contains(&p.seconds)) {
            return Err(format!("{} seconds is no place in a show", p.seconds));
        }
        self.resume = Some(p);
        Ok(())
    }
}

impl Sequence {
    /// Seconds into the show on its own clock at tick `now`, `None` while it is off: started when
    /// `run` turns on or the clock went back, held before a holding step's way out, moved on by
    /// a `go` that `rose`, wrapped by `looping`.
    fn own_clock(
        &mut self,
        on: bool,
        back: bool,
        now: Tick,
        rose: bool,
        show: &Show,
        looping: bool,
    ) -> Option<f64> {
        let resumed = self.resume(on, show);
        // A `go` that is up after the restart is no press.
        let rose = rose && !resumed;
        if !on {
            self.anchor = None;
        } else if !resumed && (self.anchor.is_none() || back) {
            self.anchor = Some((now, 0.0));
            self.free = 0;
            self.round = false;
        }
        let length = show.length();
        let (tick, base) = self.anchor?;
        let mut t = base + Tick(now.0.saturating_sub(tick.0)).seconds();
        if looping && t >= length {
            t = t.rem_euclid(length);
            self.free = 0;
            self.round = true;
            self.anchor = Some((now, t));
        }
        if let Some(at) = show.held(t, self.free) {
            t = at;
            self.anchor = Some((now, t));
        }
        if rose && let Some((from, free)) = show.go(t) {
            t = from;
            self.free = self.free.max(free);
            self.anchor = Some((now, t));
        }
        Some(t.min(length))
    }

    /// Takes the place kept before a restart, once: when the show is on and is still the show the
    /// place was kept for. Returns whether it did; otherwise the show starts as usual.
    fn resume(&mut self, on: bool, show: &Show) -> bool {
        let Some(p) = self.resume.take() else { return false };
        let same = self.show.as_ref().is_some_and(|(s, _)| s.fingerprint() == p.show);
        if !on || !same {
            log::info!(
                "chan.sequence: the place kept from before the restart is not taken ({})",
                if on { "the show has changed" } else { "the show is off" }
            );
            return false;
        }
        let Some(now) = self.last else { return false };
        self.anchor = Some((now, p.seconds.min(show.length())));
        self.free = p.free.min(show.steps.len());
        self.round = p.round;
        true
    }
}

/// The entries a `chan.sequence`'s channels say are shown: `from`, `to` and the share of `to`.
/// Without those channels: black.
pub fn shown(chans: Option<&Chans>) -> (Entry, Entry, f64) {
    let Some(c) = chans else { return (Entry::Black, Entry::Black, 0.0) };
    let from = c.value(Some("from")).map_or(Entry::Black, Entry::from_code);
    let to = c.value(Some("to")).map_or(from, Entry::from_code);
    let mix = c.value(Some("mix")).filter(|m| m.is_finite()).map_or(0.0, |m| m.clamp(0.0, 1.0));
    if mix < MIX_EPSILON { (from, from, 0.0) } else { (from, to, mix) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = "# a show of three pieces\n\
                        a 10 fade 2\n\
                        b 10 black 1   # out to black, c cuts in\n\
                        c 5 flash 0.5\n";

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_show_with_every_transition() {
        let show = Show::new(SHOW, "", 1.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        assert_eq!(show.names(), ["a", "b", "c"]);
        // Lead-in 1, a 1..11, b 11..21, c 21..26, white fading to black 26..26.5.
        assert!(close(show.length(), 26.5));
        let (a, b, c) = (Entry::Piece(0), Entry::Piece(1), Entry::Piece(2));
        let at = |t: f64| show.at(t);
        let m = at(0.5);
        assert_eq!((m.step, m.piece, m.from, m.to, m.mix), (-1, None, Entry::Black, Entry::Black, 0.0));
        // The cut into the first piece at the end of the lead-in.
        let m = at(1.0);
        assert_eq!((m.step, m.piece, m.from, m.mix), (0, Some(0), a, 0.0));
        assert!(m.shows(0) && !m.shows(1));
        // The cross-fade over a's last two seconds: both shown, b's share growing.
        assert_eq!(at(9.0).mix, 0.0);
        let m = at(10.0);
        assert_eq!((m.step, m.from, m.to), (0, a, b));
        assert!(close(m.mix, 0.5));
        assert!(m.shows(0) && m.shows(1) && !m.shows(2));
        // b alone from 11 on: its step begins when the fade ends.
        let m = at(11.0);
        assert_eq!((m.step, m.piece, m.from, m.to, m.mix), (1, Some(1), b, b, 0.0));
        assert!(!m.shows(0));
        // Out to black over b's last second, then c cuts in.
        let m = at(20.5);
        assert_eq!((m.from, m.to), (b, Entry::Black));
        assert!(close(m.mix, 0.5));
        assert!(m.shows(1));
        let m = at(21.0);
        assert_eq!((m.step, m.from, m.mix), (2, c, 0.0));
        assert!(!m.shows(1) && m.shows(2));
        // The flash: white at c's end, fading into the black the show ends in.
        let m = at(26.0);
        assert_eq!((m.step, m.piece, m.from, m.mix), (3, None, Entry::White, 0.0));
        assert!(!m.shows(2));
        let m = at(26.25);
        assert_eq!((m.from, m.to), (Entry::White, Entry::Black));
        assert!(close(m.mix, 0.5));
        let m = at(100.0);
        assert_eq!((m.step, m.from, m.mix), (3, Entry::Black, 0.0));
    }

    #[test]
    fn a_flash_fades_into_the_next_piece() {
        let show = Show::new("a 4 flash 1\nb 4", "", 0.0);
        let m = show.at(4.0);
        assert_eq!((m.step, m.piece, m.from), (1, Some(1), Entry::White));
        assert!(!m.shows(0) && !m.shows(1));
        let m = show.at(4.5);
        assert_eq!((m.from, m.to), (Entry::White, Entry::Piece(1)));
        assert!(m.shows(1), "the next piece runs from the start of its fade");
        assert_eq!(show.at(5.0).from, Entry::Piece(1));
        // No lead-in: the first piece from the first moment.
        assert_eq!(show.at(0.0).from, Entry::Piece(0));
    }

    #[test]
    fn a_piece_twice_in_a_row_is_shown_throughout() {
        let show = Show::new("a 2\na 2\nb 2", "", 0.0);
        assert!((0..40).map(|i| f64::from(i) * 0.1).all(|t| show.at(t).shows(0)));
        assert_eq!(show.at(2.0).step, 1);
    }

    #[test]
    fn names_fix_the_numbers_and_leave_out_others() {
        let show = Show::new("b 5\nx 5\na 5 sideways", "a b c", 1.0);
        assert_eq!(show.names(), ["a", "b", "c"]);
        assert_eq!(show.at(1.0).from, Entry::Piece(1));
        assert_eq!(show.at(6.0).from, Entry::Piece(0), "x left out, so a follows b");
        let problems = show.problems().join("\n");
        assert!(problems.contains("no piece `x`") && problems.contains("sideways"), "{problems}");
    }

    #[test]
    fn json_as_in_a_show_file() {
        let json = r#"{"lead_in": 0.5, "steps": [
            {"piece": "a", "duration": 3, "transition": "fade", "length": 1},
            {"piece": "b", "duration": 3, "transition": "black"},
            {"duration": 3}
        ]}"#;
        let show = Show::new(json, "", 9.0);
        assert_eq!(show.at(0.5).from, Entry::Piece(0), "the file's lead-in wins");
        assert!(close(show.at(3.0).mix, 0.5));
        // black's default length is 1.5 s: b fades out over 5 to 6.5.
        assert!(close(show.at(5.75).mix, 0.5));
        assert_eq!(show.problems().len(), 1, "{:?}", show.problems());
        assert!(!Show::new("{ nope", "", 0.0).problems().is_empty());
    }

    #[test]
    fn bad_lines_are_left_out_and_nothing_breaks() {
        let show =
            Show::new("a ten\nb 1e400\n\n   # only a comment\nc -5 fade -1\nd 3 fade 1 extra", "", -3.0);
        assert_eq!(show.names(), ["c", "d"]);
        assert_eq!(show.problems().len(), 3, "{:?}", show.problems());
        // Clamped: no lead-in below 0, steps of at least 0.1 s, fades of at least 0.05 s.
        assert_eq!(show.at(0.0).from, Entry::Piece(0));
        for t in [-1.0, 0.05, 0.1, 1.0, f64::NAN, f64::INFINITY] {
            let m = show.at(t);
            assert!(m.mix.is_finite() && (0.0..=1.0).contains(&m.mix));
        }
        let empty = Show::new("", "", 1.0);
        assert_eq!(empty.at(5.0).from, Entry::Black);
        assert_eq!(empty.problems(), ["no steps"]);
        let odd = Show::new("a-b 3", "", 0.0);
        assert!(odd.problems()[0].contains("no channel"));
    }

    #[test]
    fn codes_and_what_is_shown() {
        for e in [Entry::Black, Entry::White, Entry::Piece(0), Entry::Piece(7)] {
            assert_eq!(Entry::from_code(e.code()), e);
        }
        assert_eq!(Entry::from_code(f64::NAN), Entry::Black);
        assert_eq!(Entry::from_code(-7.0), Entry::Black);
        assert_eq!(shown(None), (Entry::Black, Entry::Black, 0.0));
    }

    #[test]
    fn a_holding_step_waits_before_its_way_out() {
        // a holds, fades into b over 2 s; b holds behind a flash in; c is timed.
        let show = Show::new("a hold fade 2\nb hold flash 0.5\nc 4\nd hold", "", 1.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        // a from 1 s; long enough for its fade and a moment: 2.1 s, its way out from 1.1 s.
        let (a_exit, hold) = show.exits[0];
        assert!(hold && close(a_exit, 1.1));
        let at = show.held(5.0, 0).unwrap();
        assert!(close(at, 1.1 - HOLD_BEFORE));
        assert_eq!(show.held(1.0, 0), None, "not there yet");
        let m = show.at(at);
        assert_eq!((m.step, m.from, m.mix), (0, Entry::Piece(0), 0.0));
        // Let go: from there the fade plays, and b's hold comes next.
        assert_eq!(show.go(at), Some((at, 1)));
        assert!(show.at(at + HOLD_BEFORE + 1.0).mix > 0.4);
        let b_hold = show.held(100.0, 1).unwrap();
        let m = show.at(b_hold);
        assert_eq!((m.step, m.from), (1, Entry::Piece(1)));
        // b's way out is the flash: its cut to white has not happened while it holds.
        assert_eq!(show.at(b_hold + 2.0 * HOLD_BEFORE).from, Entry::White);
        // During a fade `go` does nothing; in a timed step it jumps to the step's way out.
        assert_eq!(show.go(at + 1.0), None);
        let c_start = show.starts[2];
        let (c_exit, c_holds) = show.exits[2];
        assert!(!c_holds && close(c_exit, c_start + 4.0));
        let (from, free) = show.go(c_start + 0.5).unwrap();
        assert!(close(from, c_exit - HOLD_BEFORE) && free == 3);
        // In the lead-in it starts the first step; after the last, nothing.
        assert_eq!(show.go(0.5), Some((1.0, 0)));
        assert_eq!(show.go(1000.0), None);
        // JSON says it as a duration.
        let json = Show::new(r#"{"steps": [{"piece": "a", "duration": "hold"}]}"#, "", 0.0);
        assert!(json.exits[0].1 && json.problems().is_empty());
    }

    /// The node over time: it holds on the first cue, `go` lets it fade into the next, a second
    /// `go` during the fade is ignored, and a timed step follows by itself.
    #[test]
    fn go_steps_a_cue_list() {
        use penumbra_core::{NodePath, ParamPath, Rate, ShowClock, Value};
        use penumbra_graph::{Cooker, JobPool, Network, Op, Port};
        let types = Arc::new(crate::builtin_types().unwrap());
        let create = Op::Create {
            path: NodePath::parse("/s").unwrap(),
            kind: SEQUENCE.into(),
            params: Default::default(),
        };
        let set = |p: &str, v: Value| Op::Set { param: ParamPath::parse(p).unwrap(), value: v };
        let mut net = Arc::new(
            Network::new()
                .apply(
                    &types,
                    &[
                        create,
                        set("/s.steps", "a hold fade 1\nb 2 fade 1\nc hold".into()),
                        set("/s.lead_in", Value::Float(0.0)),
                    ],
                )
                .unwrap()
                .network,
        );
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/s".parse().unwrap()).unwrap();
        let mut clock = ShowClock::new(Rate::FPS_60, Default::default());
        let mut run = |net: &Arc<Network>, frames: u64| {
            for _ in 0..frames {
                clock.advance(1);
                cooker.cook(net, clock.now(), &[id], &[]);
            }
            let c = cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone();
            (c.value(Some("step")).unwrap(), c.value(Some("from")).unwrap(), c.value(Some("mix")).unwrap())
        };
        let go = |net: &mut Arc<Network>, v: f64| {
            *net = Arc::new(net.apply(&types, &[set("/s.go", Value::Float(v))]).unwrap().network);
        };
        // Ten seconds on: still a, no fade.
        assert_eq!(run(&net, 600), (0.0, 0.0, 0.0));
        go(&mut net, 1.0);
        let (step, from, mix) = run(&net, 30);
        assert_eq!((step, from), (0.0, 0.0));
        assert!((mix - 0.5).abs() < 0.05, "half way through the fade: {mix}");
        // Held down and pressed again during the fade: nothing more.
        go(&mut net, 0.0);
        run(&net, 1);
        go(&mut net, 1.0);
        assert_eq!(run(&net, 40).0, 1.0, "b, not skipped");
        // b plays 2 s and fades into c by itself; c holds.
        assert_eq!(run(&net, 300), (2.0, 2.0, 0.0));
        go(&mut net, 0.0);
        assert_eq!(run(&net, 300), (2.0, 2.0, 0.0));
    }

    /// A `clock` from outside sets the show's seconds outright: jumps both ways land on the step
    /// at those seconds, holds and `go` do nothing, `run` off is black, a looping show wraps, and
    /// no place is kept. Back to -1 the show runs on its own clock again, from its start.
    #[test]
    fn an_outside_clock_sets_the_place_in_the_show() {
        use penumbra_core::{NodePath, ParamPath, Rate, ShowClock, Value};
        use penumbra_graph::{Cooker, JobPool, Network, Op, Port};
        let types = Arc::new(crate::builtin_types().unwrap());
        let create = Op::Create {
            path: NodePath::parse("/s").unwrap(),
            kind: SEQUENCE.into(),
            params: Default::default(),
        };
        let set = |p: &str, v: Value| Op::Set { param: ParamPath::parse(p).unwrap(), value: v };
        let mut net = Arc::new(
            Network::new()
                .apply(
                    &types,
                    &[
                        create,
                        set("/s.steps", "a 10 fade 2\nb hold\nc 5".into()),
                        set("/s.lead_in", Value::Float(0.0)),
                    ],
                )
                .unwrap()
                .network,
        );
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/s".parse().unwrap()).unwrap();
        let mut clock = ShowClock::new(Rate::FPS_60, Default::default());
        let kept = std::cell::Cell::new(false);
        let mut at = |net: &mut Arc<Network>, ops: &[Op]| {
            *net = Arc::new(net.apply(&types, ops).unwrap().network);
            clock.advance(1);
            cooker.cook(net, clock.now(), &[id], &[]);
            kept.set(cooker.show_places().iter().any(|(n, _)| *n == id));
            let c = cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone();
            let v = |n: &str| c.value(Some(n)).unwrap();
            (v("step"), v("mix"), v("seconds"), v("done"))
        };
        let clock_at = |t: f64| set("/s.clock", Value::Float(t));
        // b holds on its own clock; from outside its 0.1 s pass, and c follows.
        assert_eq!(at(&mut net, &[clock_at(10.05)]).0, 1.0);
        assert_eq!(at(&mut net, &[clock_at(10.5)]).0, 2.0);
        // Back and forth, and the fade at 9 s.
        assert_eq!(at(&mut net, &[clock_at(3.0)]), (0.0, 0.0, 3.0, 0.0));
        let (step, mix, ..) = at(&mut net, &[clock_at(9.0)]);
        assert!(step == 0.0 && (mix - 0.5).abs() < 1e-9);
        assert_eq!(at(&mut net, &[clock_at(12.0)]).0, 2.0);
        // `go` changes nothing.
        assert_eq!(at(&mut net, &[set("/s.go", Value::Float(1.0)), clock_at(3.0)]).0, 0.0);
        assert_eq!(at(&mut net, &[set("/s.go", Value::Float(0.0))]).0, 0.0);
        // Past the end: done, the number of steps; a looping show wraps.
        assert_eq!(at(&mut net, &[clock_at(100.0)]), (3.0, 0.0, 15.1, 1.0));
        let (step, _, seconds, done) = at(&mut net, &[set("/s.mode", "loop".into())]);
        assert_eq!(step, 0.0);
        assert!((seconds - 100.0f64.rem_euclid(15.1)).abs() < 1e-9 && done == 1.0);
        // Off: black.
        let (step, ..) = at(&mut net, &[set("/s.run", Value::Float(0.0))]);
        assert_eq!(step, -1.0);
        at(&mut net, &[set("/s.run", Value::Float(1.0)), set("/s.mode", "once".into())]);
        assert!(!kept.get(), "no place with a clock from outside");
        // Its own clock again: the show starts over and b holds.
        assert_eq!(at(&mut net, &[clock_at(-1.0)]).2, 0.0);
        for _ in 0..700 {
            at(&mut net, &[]);
        }
        assert_eq!(at(&mut net, &[]).0, 1.0);
        assert!(kept.get(), "its own clock keeps its place");
    }

    /// The place in a show is kept and taken back by a new operator (a restart): it goes on at
    /// the same cue, with the holds let go before still let go; a place for another show, or
    /// one that does not parse, is not taken.
    #[test]
    fn a_kept_place_goes_on_at_the_same_cue() {
        use penumbra_core::{NodePath, ParamPath, Rate, ShowClock, Value};
        use penumbra_graph::{Cooker, JobPool, Network, Op, Port};
        let types = Arc::new(crate::builtin_types().unwrap());
        let set = |p: &str, v: Value| Op::Set { param: ParamPath::parse(p).unwrap(), value: v };
        let net = |go: f64| {
            let create = Op::Create {
                path: NodePath::parse("/s").unwrap(),
                kind: SEQUENCE.into(),
                params: Default::default(),
            };
            Arc::new(
                Network::new()
                    .apply(
                        &types,
                        &[
                            create,
                            set(
                                "/s.steps",
                                "a hold
b 5
c hold"
                                    .into(),
                            ),
                            set("/s.lead_in", Value::Float(0.0)),
                            set("/s.go", Value::Float(go)),
                        ],
                    )
                    .unwrap()
                    .network,
            )
        };
        let (up, down) = (net(1.0), net(0.0));
        let id = down.resolve(&"/s".parse().unwrap()).unwrap();
        let cook = |cooker: &mut Cooker, clock: &mut ShowClock, net: &Arc<Network>, frames: u64| {
            for _ in 0..frames {
                clock.advance(1);
                cooker.cook(net, clock.now(), &[id], &[]);
            }
            let c = cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone();
            (c.value(Some("step")).unwrap(), c.value(Some("seconds")).unwrap())
        };
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let mut clock = ShowClock::new(Rate::FPS_60, Default::default());
        cook(&mut cooker, &mut clock, &down, 60);
        cook(&mut cooker, &mut clock, &up, 1);
        // 2.5 s into b's 5 s.
        let (step, seconds) = cook(&mut cooker, &mut clock, &down, 150);
        assert_eq!(step, 1.0);
        let places = cooker.show_places();
        let (_, place) = places.iter().find(|(n, _)| *n == id).unwrap().clone();
        let p: Place = serde_json::from_str(&place).unwrap();
        assert_eq!(p.free, 1);
        assert!(p.seconds <= seconds && p.seconds >= seconds - 1.0, "whole seconds while it runs: {p:?}");

        // A restart: a new cooker and clock, the place given back before the first cook.
        let restarted = |place: &str| {
            let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
            let mut clock = ShowClock::new(Rate::FPS_60, Default::default());
            cooker.restore_show_place(id, place.to_string()).unwrap();
            let first = cook(&mut cooker, &mut clock, &up, 1);
            (cooker, clock, first)
        };
        let (mut again, mut clock2, (step, at)) = restarted(&place);
        assert_eq!(step, 1.0, "b again, not a; `go` up after the restart is no press");
        assert!((at - p.seconds).abs() < 0.1);
        // b runs out by itself and c holds.
        assert_eq!(cook(&mut again, &mut clock2, &down, 400).0, 2.0);
        // Another show's place, or none that parses: from the start.
        let other = place.replace(&p.show, "0000000000000000");
        assert_eq!(restarted(&other).2.0, 0.0);
        let mut bad = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let mut clock3 = ShowClock::new(Rate::FPS_60, Default::default());
        cook(&mut bad, &mut clock3, &down, 1);
        assert!(bad.restore_show_place(id, r#"{"seconds": -1}"#.into()).is_err());
        assert!(bad.restore_show_place(id, "[nan".into()).is_err());
        let huge = format!(r#"{{"show": "{}", "seconds": 1e300, "free": 0, "round": false}}"#, p.show);
        assert!(bad.restore_show_place(id, huge).is_err());
        assert_eq!(cook(&mut bad, &mut clock3, &down, 1).0, 0.0);
    }
}
