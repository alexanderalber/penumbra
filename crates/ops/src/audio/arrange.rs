//! An arrangement of a music stage from the sections of a sound file: one row per section that
//! says how present each element is, where the colour stands between warm and cold, and how
//! intense the picture is.
//!
//! The rules follow how strongly the music changes at each boundary (`change` of the music bus),
//! not what kind of section comes: while the music stays alike the picture stays alike, and a
//! bigger change in the music changes more of the picture. A picture may come back; it only has
//! to differ from the one before at a boundary.
//!
//! - The elements come in slots (floor, sky, ...). Per slot one element is on. The first
//!   section shows the first element of each slot.
//! - At a boundary below [`CHANGE_SMALL`] nothing swaps. From [`CHANGE_SMALL`] one slot swaps to
//!   its next element; the slots take turns. From [`CHANGE_BIG`] every slot swaps.
//! - **Dwell:** an element on for [`DWELL`] sections or more (the caller may give another
//!   count) gives way at the next boundary from [`CHANGE_SMALL`], whether or not it is its slot's
//!   turn. A slot counts on while its stage hides it, so it shows its next element when the stage
//!   comes back. Without this a slot that shows on one stage only would swap a handful of times.
//! - **Stages per element:** an element may name the stages it shows in. In a row whose stage
//!   is not among them it stays at presence 0, and the slot's swap passes it by; when the stage
//!   changes to one it does not show in, its slot takes its next element there.
//! - **Weights:** an element of weight 3 comes on three times as often as one of weight 1. The
//!   next element is the one owed the most turns (smooth weighted round robin), so the turns of a
//!   heavy element spread out evenly and the same file always gives the same table. With equal
//!   weights the slot goes through its elements in order.
//! - The colour walks on at every boundary by `change` times [`WALK_STEP`] (twice that from
//!   [`CHANGE_BIG`]) and never walks back: a place grows, and `warmth` goes 0, 1, 0, ... along it.
//! - The role of a section sets the rest: calm in an intro or outro, an empty floor in a break,
//!   everything on in a drop. The sky and the mid slot stay on through a break, dimmed.
//! - The slot named [`POST`] holds effects over the finished picture. Each of its elements is on
//!   or off by itself, so several can be on at once: all are off in an intro, outro or break, all
//!   come on at a drop, and a swap of the post slot switches its next element on or off.
//! - **Emptiness:** at a strong boundary into a main section, after at least [`EMPTY_AFTER`]
//!   full sections, a section may keep the stage and one slot and nothing else. Whether it does
//!   is drawn from the section's seed, with a chance of [`EMPTY_CHANCE`] times `change`, so the
//!   same file always gives the same table.
//! - **Camera:** each row names one of the stage's camera presets ([`CAMERA_MAIN`] and the
//!   others): low flight in a main section, climbing in a build, high and wide on a drop, still in
//!   a break, and a variant per track in an intro, an outro and a main section that starts a track.
//! - **Stage:** with several stages, the row says which one shows. It changes only where a track
//!   starts with a `change` of at least [`STAGE_CHANGE`], and at most once per [`STAGE_GAP`].
//!   It walks to the next stage and back again (0, 1, 2, 1, 0, 1, ...), so every change goes to a
//!   neighbour in the list: a blending switch fades between the two and never through a third.
//! - **Opening:** the file's first section brings its slots in one after another, at bar starts:
//!   first the sky, then the mid slot, then the floor and the other slots, the ceiling last. It
//!   is split into one row per step ([`opening`]), at least [`OPENING_BARS`] bars each, and each
//!   step fades in over a bar. The steps count as one section for the dwell.
//! - **Gaps:** the row says how much of the floor breaks off into gaps: [`GAPS_BUILD`] in a
//!   build, [`GAPS_BREAK`] in a break, none elsewhere. The caller may give other shares
//!   ([`Gaps`]).
//!
//! The rows go into a `table.text` and a `chan.sequence` with one step per row, its clock on the
//! file's seconds, so the user can edit the table by hand and the picture follows the edit.

use penumbra_audio::beats::Role;
use penumbra_core::Name;
use penumbra_core::param::MAX_TEXT;

/// A boundary below this changes no element; the colour still walks on.
pub const CHANGE_SMALL: f64 = 0.3;
/// A boundary from this on swaps every slot that has more than one element, and the colour
/// walks a double step. Track starts and most drops reach it.
pub const CHANGE_BIG: f64 = 0.7;
/// How far the colour walks per unit of `change`: a full change moves `warmth` a quarter of the
/// way from warm to cold.
pub const WALK_STEP: f64 = 0.25;
/// The presence of the elements outside the sky in an intro or outro.
pub const CALM_PRESENCE: f64 = 0.4;
/// The intensity of an intro or outro.
pub const CALM_INTENSITY: f64 = 0.3;
/// The presence of the sky element in a break.
pub const BREAK_SKY: f64 = 0.6;
/// The shortest fade from one row into the next, in seconds, unless the caller gives another.
pub const FADE_MIN: f64 = 2.0;
/// The shortest fade into a row whose stage differs from the row before, in seconds, unless the
/// caller gives another: long enough to see one stage turn into the next.
pub const STAGE_FADE: f64 = 4.0;
/// The sections an element stays on before it gives way at the next boundary from
/// [`CHANGE_SMALL`], unless the caller gives another count.
pub const DWELL: usize = 3;
/// The longest `dwell` taken, in sections.
pub const DWELL_MAX: usize = 1000;
/// The longest `fade_min` taken, in seconds.
pub const FADE_MIN_MAX: f64 = 30.0;
/// The slot the role rules treat as the sky; every other slot but [`MID`] follows the floor's rules.
pub const SKY: &str = "sky";
/// The slot in front of the camera (a glider, particles). It follows the sky's rules: it stays
/// through a break at [`BREAK_SKY`] and is fully there in an intro or outro, since an empty
/// space before the camera reads as a gap more than an empty floor does.
pub const MID: &str = "mid";
/// The slot of effects over the finished picture (scanlines, ...). Its elements are on or off
/// each by itself: off in an intro, outro or break, on from a drop.
pub const POST: &str = "post";
/// The slot hanging from the ceiling. It follows the floor's rules: empty in a break.
pub const CEILING: &str = "ceiling";
/// Full sections (main, build or drop, not emptied) a row needs behind it before it may empty.
pub const EMPTY_AFTER: usize = 3;
/// The chance of an empty section at a strong boundary, times its `change`.
pub const EMPTY_CHANCE: f64 = 0.5;
/// The camera presets a stage has, by index. A stage holds this many rows of presets.
pub const CAMERA_PRESETS: usize = 6;
/// Low flight: a main section.
pub const CAMERA_MAIN: usize = 0;
/// Higher and looking down: a build.
pub const CAMERA_BUILD: usize = 1;
/// High and wide: a drop.
pub const CAMERA_DROP: usize = 2;
/// Still and level: a break.
pub const CAMERA_BREAK: usize = 3;
/// The variants the tracks take in turn: an intro, an outro, a main section at a track start.
pub const CAMERA_VARIANTS: [usize; 2] = [4, 5];
/// The least `change` at a track start that may switch the stage.
pub const STAGE_CHANGE: f64 = 0.8;
/// The least time between two switches of the stage, in seconds; the file's start counts as one.
pub const STAGE_GAP: f64 = 480.0;
/// The fewest bars a step of the opening lasts ([`opening`]).
pub const OPENING_BARS: usize = 2;
/// The share of the floor in gaps during a build.
pub const GAPS_BUILD: f64 = 0.3;
/// The share of the floor in gaps during a break.
pub const GAPS_BREAK: f64 = 0.6;

/// How much of the floor breaks off into gaps, per role: a share from 0 to 1 for a build and a
/// break; every other role keeps its floor whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gaps {
    pub build: f64,
    pub brk: f64,
}

impl Default for Gaps {
    fn default() -> Gaps {
        Gaps { build: GAPS_BUILD, brk: GAPS_BREAK }
    }
}

/// What the caller may set of the rules: the floor's gaps, the dwell and the opening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    pub gaps: Gaps,
    /// The sections an element stays on before it gives way at the next boundary from
    /// [`CHANGE_SMALL`]; 0 turns the rule off.
    pub dwell: usize,
    /// How many of the first parts are the steps of the opening ([`opening`]): their rows bring
    /// the slots in one after another. 0 or 1: no opening.
    pub opening: usize,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules { gaps: Gaps::default(), dwell: DWELL, opening: 0 }
    }
}

/// The order the opening brings the slots in, as indices into `slots`: the sky, the mid slot,
/// the others in their order, the ceiling last. The post slot is left out: it is off in an
/// intro anyway.
fn opening_order(slots: &[Slot]) -> Vec<usize> {
    let rank = |s: &Slot| match s.name.as_str() {
        SKY => 0,
        MID => 1,
        CEILING => 3,
        _ => 2,
    };
    let mut order: Vec<usize> =
        (0..slots.len()).filter(|&i| slots.get(i).is_some_and(|s| s.name != POST)).collect();
    order.sort_by_key(|&i| slots.get(i).map_or(0, rank));
    order
}

/// The parts with the first one split into the steps of the opening: one step per slot the
/// opening brings in (fewer when the first part has fewer than [`OPENING_BARS`] bars per step),
/// each beginning on one of `bars` (the bar starts, seconds) and fading in over that bar. Also the
/// number of steps, the `opening` of [`Rules`]: 1 where the first part stays whole.
pub fn opening(parts: &[Part], slots: &[Slot], bars: &[f64]) -> (Vec<Part>, usize) {
    let Some(first) = parts.first().copied() else { return (Vec::new(), 0) };
    let inside: Vec<f64> =
        bars.iter().copied().filter(|b| b.is_finite() && *b > first.start && *b < first.end).collect();
    let wanted = opening_order(slots).len();
    let n = wanted.min((inside.len() + 1) / OPENING_BARS);
    if n < 2 {
        return (parts.to_vec(), 1);
    }
    let mut out = Vec::with_capacity(parts.len() + n - 1);
    let mut start = first.start;
    let mut zone = first.zone;
    for k in 1..=n {
        let end = if k == n {
            first.end
        } else {
            // `as`: small counts.
            let at = ((k * inside.len()) as f64 / n as f64).round() as usize;
            inside.get(at.min(inside.len() - 1)).copied().unwrap_or(first.end)
        };
        out.push(Part { start, end, zone, change: if k == 1 { first.change } else { 0.0 }, ..first });
        // The next step fades in over the bar it begins with.
        let bar_end = bars.iter().copied().find(|b| *b > end).unwrap_or(end).min(first.end);
        zone = (end, bar_end);
        start = end;
    }
    for (k, p) in out.iter_mut().enumerate() {
        if k > 0 {
            p.track_start = false;
        }
    }
    out.extend(parts.iter().skip(1).copied());
    (out, n)
}

impl Gaps {
    /// The share of the floor in gaps for a section of `role`.
    pub fn of(&self, role: Role) -> f64 {
        match role {
            Role::Build => unit(self.build),
            Role::Break => unit(self.brk),
            Role::Intro | Role::Outro | Role::Main | Role::Drop => 0.0,
        }
    }
}
/// The columns before the elements and after them.
pub const LEADING: [&str; 4] = ["start", "end", "role", "change"];
pub const TRAILING: [&str; 5] = ["warmth", "intensity", "camera", "stage", "gaps"];
/// The piece every step names: the rows differ by their table row, not by a piece.
pub const PIECE: &str = "row";

/// Shortest step written, in milliseconds (`chan.sequence` lengthens shorter ones).
const MIN_STEP_MS: i64 = 100;
/// Shortest blend zone taken as one, in milliseconds; a shorter one is a cut.
const MIN_ZONE_MS: i64 = 50;

/// One section as the arranger sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Part {
    /// Seconds into the file.
    pub start: f64,
    pub end: f64,
    /// Where the section before blends into this one, seconds; equal at a cut.
    pub zone: (f64, f64),
    pub role: Role,
    /// How strongly the music changes where the section begins, 0 to 1.
    pub change: f64,
    /// Its mean energy, 0 to 1.
    pub energy: f64,
    /// It begins a track.
    pub track_start: bool,
    /// A number fixed per kind of section, for the draws of the arrangement.
    pub seed: u32,
}

/// A spatial layer of the stage and its interchangeable elements, in the order they come on.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub name: String,
    /// The elements' column names.
    pub elements: Vec<String>,
    /// How often each element comes on, against the others in the slot: one per element, or
    /// empty for 1 each. A weight that is no number above 0 counts as 1. The post slot ignores
    /// them (its elements are on or off each by itself).
    pub weights: Vec<f64>,
    /// The stages each element shows in, by index: one list per element, or none for every
    /// stage. An empty list counts as every stage.
    pub stages: Vec<Vec<usize>>,
}

/// The heaviest weight an element takes; heavier ones count as this.
pub const WEIGHT_MAX: f64 = 100.0;

impl Slot {
    /// A slot whose elements weigh 1 each.
    pub fn new(name: &str, elements: &[&str]) -> Slot {
        Slot {
            name: name.into(),
            elements: elements.iter().map(|e| e.to_string()).collect(),
            weights: Vec::new(),
            stages: Vec::new(),
        }
    }

    /// Whether element `i` shows on stage `stage`.
    pub fn shows_in(&self, i: usize, stage: usize) -> bool {
        match self.stages.get(i) {
            Some(list) if !list.is_empty() => list.contains(&stage),
            _ => true,
        }
    }

    /// The weight of element `i`: its own, 1 where none or none usable is given.
    pub fn weight(&self, i: usize) -> f64 {
        match self.weights.get(i) {
            Some(w) if w.is_finite() && *w > 0.0 => w.min(WEIGHT_MAX),
            _ => 1.0,
        }
    }
}

/// One row of the arrangement.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub start: f64,
    pub end: f64,
    pub role: Role,
    pub change: f64,
    /// One per element, in the order of [`columns`].
    pub presence: Vec<f64>,
    pub warmth: f64,
    pub intensity: f64,
    /// Where the colour's walk stands; `warmth` is its triangle wave.
    pub walk: f64,
    /// The stage's camera preset, 0 to [`CAMERA_PRESETS`] - 1.
    pub camera: usize,
    /// Which stage shows, counted from 0.
    pub stage: usize,
    /// The share of the floor in gaps, 0 to 1.
    pub gaps: f64,
    /// The section keeps the stage and one slot only.
    pub empty: bool,
}

/// A number that is none counts as 0; the rest is clamped to 0..1.
fn unit(x: f64) -> f64 {
    if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }
}

/// The element columns of `slots`, slot after slot.
pub fn columns(slots: &[Slot]) -> Vec<String> {
    slots.iter().flat_map(|s| s.elements.iter().cloned()).collect()
}

/// Why `slots` cannot make a table: an element name that is no channel name, a name twice or
/// one of the table's own columns, or no element at all.
pub fn check(slots: &[Slot]) -> Result<(), String> {
    let cols = columns(slots);
    if cols.is_empty() {
        return Err("no elements: give at least one slot with one element".into());
    }
    if let Some(s) = slots.iter().find(|s| s.elements.is_empty()) {
        return Err(format!("slot `{}` has no elements", s.name));
    }
    for (i, c) in cols.iter().enumerate() {
        if Name::new(c).is_err() {
            return Err(format!("`{c}` makes no column name (letters, digits and _)"));
        }
        if LEADING.contains(&c.as_str()) || TRAILING.contains(&c.as_str()) {
            return Err(format!("an element may not be called `{c}`: the table has a column of that name"));
        }
        if cols.iter().take(i).any(|d| d == c) {
            return Err(format!("`{c}` is named twice: every element needs a name of its own"));
        }
    }
    Ok(())
}

/// The triangle wave of the walk: 0 at 0, 1 at 1, 0 at 2, ...
pub fn warmth_at(walk: f64) -> f64 {
    if !walk.is_finite() {
        return 0.0;
    }
    let p = walk.rem_euclid(2.0);
    if p <= 1.0 { p } else { 2.0 - p }
}

/// A draw from 0 to 1, fixed by a section's seed and its row (splitmix64).
pub fn draw(seed: u32, row: usize) -> f64 {
    let mut z = (u64::from(seed) << 32 ^ row as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // The top 53 bits as a fraction; `as`: exact for 53 bits.
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// What one slot shows while the rows are made.
enum State {
    /// One element on: which, the elements' weights, and the turns each is owed.
    One { on: usize, weights: Vec<f64>, owed: Vec<f64> },
    /// The post slot: each element on or off, and which one a swap switches next.
    Each(Vec<bool>, usize),
}

impl State {
    fn swappable(&self) -> bool {
        match self {
            State::One { weights, .. } => weights.len() > 1,
            State::Each(on, _) => !on.is_empty(),
        }
    }

    /// A slot with one element on, the first, as though the round robin had just picked it.
    fn one(weights: Vec<f64>) -> State {
        let total: f64 = weights.iter().sum();
        let mut owed = weights.clone();
        if let Some(first) = owed.first_mut() {
            *first -= total;
        }
        State::One { on: 0, weights, owed }
    }

    /// One swap: the next element on; in the post slot the next element switched.
    /// The next element is the one owed the most turns among those `shows` lets on, never the one
    /// on; ties go to the first.
    fn swap(&mut self, shows: &dyn Fn(usize) -> bool) {
        match self {
            State::One { on, weights, owed } => {
                // A slot with nothing else to show keeps its element and its owed turns.
                if !(0..weights.len()).any(|j| j != *on && shows(j)) {
                    return;
                }
                let total: f64 = weights.iter().sum();
                for (o, w) in owed.iter_mut().zip(weights.iter()) {
                    *o += *w;
                }
                let mut best: Option<usize> = None;
                for (j, o) in owed.iter().enumerate() {
                    if j != *on && shows(j) && best.is_none_or(|b| owed.get(b).is_some_and(|ob| o > ob)) {
                        best = Some(j);
                    }
                }
                if let Some(b) = best {
                    if let Some(o) = owed.get_mut(b) {
                        *o -= total;
                    }
                    *on = b;
                }
            }
            State::Each(on, next) => {
                let at = *next % on.len().max(1);
                if let Some(x) = on.get_mut(at) {
                    *x = !*x;
                }
                *next += 1;
            }
        }
    }

    /// A big change: every element of the post slot switches, any other slot takes its next.
    fn swap_all(&mut self, shows: &dyn Fn(usize) -> bool) {
        match self {
            State::One { .. } => self.swap(shows),
            State::Each(on, _) => on.iter_mut().for_each(|x| *x = !*x),
        }
    }

    /// The element on, for a slot with one element on.
    fn on(&self) -> Option<usize> {
        match self {
            State::One { on, .. } => Some(*on),
            State::Each(..) => None,
        }
    }
}

/// The stage after `stage` of `stages`, walking onward or back, and the way the walk goes on: it
/// turns at either end of the list.
fn next_stage(stage: usize, onward: bool, stages: usize) -> (usize, bool) {
    let last = stages.saturating_sub(1);
    if last == 0 {
        return (0, true);
    }
    let onward = stage < last && (stage == 0 || onward);
    let next = if onward { stage + 1 } else { stage - 1 };
    (next.min(last), onward)
}

/// The rows of the arrangement, one per part, for `stages` stages side by side (0 counts as 1),
/// with the rules of [`Rules::default`].
pub fn rows(parts: &[Part], slots: &[Slot], stages: usize) -> Vec<Row> {
    rows_with(parts, slots, stages, Rules::default())
}

/// [`rows`] with the floor's gaps and the dwell by `rules`.
pub fn rows_with(parts: &[Part], slots: &[Slot], stages: usize, rules: Rules) -> Vec<Row> {
    let gaps = rules.gaps;
    let mut state: Vec<State> = slots
        .iter()
        .map(|s| {
            if s.name == POST {
                State::Each(vec![false; s.elements.len()], 0)
            } else {
                State::one((0..s.elements.len()).map(|i| s.weight(i)).collect())
            }
        })
        .collect();
    let swappable: Vec<usize> =
        state.iter().enumerate().filter(|(_, s)| s.swappable()).map(|(i, _)| i).collect();
    // The slots an empty section may keep: all but the post slot.
    let content: Vec<usize> =
        slots.iter().enumerate().filter(|(_, s)| s.name != POST).map(|(i, _)| i).collect();
    let stages = stages.max(1);
    let mut turn = 0usize;
    let mut walk = 0.0f64;
    let mut dense = 0usize;
    let mut track = 0usize;
    let mut stage = 0usize;
    let mut onward = true;
    let mut switched = 0.0f64;
    // Per slot, the sections its element has been on, hidden by the stage or not.
    let mut since = vec![0usize; slots.len()];
    // The opening's steps, and the slots each brings: step k brings the first of `order` up to
    // its share of them.
    let steps = if rules.opening >= 2 { rules.opening } else { 0 };
    let order = opening_order(slots);
    let brought = |row: usize, slot: usize| -> bool {
        if row >= steps {
            return true;
        }
        let count = ((row + 1) * order.len()).div_ceil(steps);
        order.iter().take(count).any(|&s| s == slot)
    };
    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let change = if i == 0 { 0.0 } else { unit(part.change) };
        let before: Vec<Option<usize>> = state.iter().map(State::on).collect();
        let big = change >= CHANGE_BIG;
        if i > 0 {
            walk += change * WALK_STEP * if big { 2.0 } else { 1.0 };
            if part.track_start {
                track += 1;
            }
            let start = if part.start.is_finite() { part.start } else { 0.0 };
            if stages > 1 && part.track_start && change >= STAGE_CHANGE && start - switched >= STAGE_GAP {
                (stage, onward) = next_stage(stage, onward, stages);
                switched = start;
            }
            if big {
                for &s in &swappable {
                    if let (Some(x), Some(slot)) = (state.get_mut(s), slots.get(s)) {
                        x.swap_all(&|j| slot.shows_in(j, stage));
                    }
                }
            } else if change >= CHANGE_SMALL
                && let Some(&s) = swappable.get(turn % swappable.len().max(1))
            {
                if let (Some(x), Some(slot)) = (state.get_mut(s), slots.get(s)) {
                    x.swap(&|j| slot.shows_in(j, stage));
                }
                turn += 1;
            }
            // An element on for its dwell gives way, its slot's turn or not.
            if !big && change >= CHANGE_SMALL && rules.dwell > 0 {
                for (s, (x, slot)) in state.iter_mut().zip(slots).enumerate() {
                    let unswapped = x.on().is_some() && x.on() == before.get(s).copied().flatten();
                    if unswapped && since.get(s).is_some_and(|n| *n >= rules.dwell) {
                        x.swap(&|j| slot.shows_in(j, stage));
                    }
                }
            }
        }
        // An element left on where the stage does not show it gives way to the next one there.
        for (x, slot) in state.iter_mut().zip(slots) {
            if let State::One { on, .. } = x
                && !slot.shows_in(*on, stage)
            {
                x.swap(&|j| slot.shows_in(j, stage));
            }
        }
        // The opening's steps count as one section.
        if i == 0 || i >= steps {
            for ((n, x), b) in since.iter_mut().zip(&state).zip(&before) {
                *n = if i > 0 && x.on() == *b { *n + 1 } else { 1 };
            }
        }
        // The post slot follows the role: off where it is calm, on at a drop.
        for x in &mut state {
            if let State::Each(on, _) = x {
                match part.role {
                    Role::Intro | Role::Outro | Role::Break => on.iter_mut().for_each(|o| *o = false),
                    Role::Drop => on.iter_mut().for_each(|o| *o = true),
                    _ => {}
                }
            }
        }
        let empty = part.role == Role::Main
            && big
            && dense >= EMPTY_AFTER
            && !content.is_empty()
            && draw(part.seed, i) < EMPTY_CHANCE * change;
        let kept = content.get(turn % content.len().max(1)).copied();
        dense = match part.role {
            Role::Main | Role::Build | Role::Drop if !empty => dense + 1,
            _ => 0,
        };
        let mut presence = Vec::new();
        for (s, (slot, x)) in slots.iter().zip(&state).enumerate() {
            let sky = slot.name == SKY || slot.name == MID;
            let level = match (part.role, sky) {
                (Role::Intro | Role::Outro, false) => CALM_PRESENCE,
                (Role::Break, false) => 0.0,
                (Role::Break, true) => BREAK_SKY,
                _ => 1.0,
            };
            let level = if (empty && Some(s) != kept) || !brought(i, s) { 0.0 } else { level };
            match x {
                State::One { on, weights, .. } => presence.extend(
                    (0..weights.len()).map(|j| if j == *on && slot.shows_in(j, stage) { level } else { 0.0 }),
                ),
                State::Each(on, _) => presence.extend(
                    on.iter()
                        .enumerate()
                        .map(|(j, o)| if *o && !empty && slot.shows_in(j, stage) { 1.0 } else { 0.0 }),
                ),
            }
        }
        let intensity = match part.role {
            Role::Intro | Role::Outro => CALM_INTENSITY,
            Role::Drop => 1.0,
            _ => unit(part.energy),
        };
        let variant = CAMERA_VARIANTS.get(track % CAMERA_VARIANTS.len()).copied().unwrap_or(CAMERA_MAIN);
        let camera = match part.role {
            Role::Break => CAMERA_BREAK,
            Role::Drop => CAMERA_DROP,
            Role::Build => CAMERA_BUILD,
            Role::Intro | Role::Outro => variant,
            Role::Main if part.track_start => variant,
            Role::Main => CAMERA_MAIN,
        };
        out.push(Row {
            start: part.start,
            end: part.end,
            role: part.role,
            change,
            presence,
            warmth: warmth_at(walk),
            intensity,
            walk,
            camera,
            stage,
            gaps: gaps.of(part.role),
            empty,
        });
    }
    out
}

/// A number for the table: at most three decimals, no trailing zeros.
fn num(x: f64) -> String {
    let x = if x.is_finite() { x } else { 0.0 };
    let mut s = format!("{x:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

/// The table as csv: a header, then one line per row.
pub fn table(slots: &[Slot], rows: &[Row]) -> String {
    let mut header: Vec<String> = LEADING.iter().map(|s| s.to_string()).collect();
    header.extend(columns(slots));
    header.extend(TRAILING.iter().map(|s| s.to_string()));
    let mut text = header.join(",");
    text.push('\n');
    for r in rows {
        let mut cells = vec![num(r.start), num(r.end), r.role.name().to_string(), num(r.change)];
        cells.extend(r.presence.iter().map(|p| num(*p)));
        cells.push(num(r.warmth));
        cells.push(num(r.intensity));
        cells.push(r.camera.to_string());
        cells.push(r.stage.to_string());
        cells.push(num(r.gaps));
        text.push_str(&cells.join(","));
        text.push('\n');
    }
    text
}

/// Seconds as whole milliseconds; anything that is no number counts as 0.
fn ms(t: f64) -> i64 {
    if t.is_finite() {
        // `as`: clamped far inside i64.
        (t * 1000.0).round().clamp(0.0, 1e15) as i64
    } else {
        0
    }
}

/// The steps for a `chan.sequence` whose clock is the file's seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Steps {
    pub text: String,
    /// Where each step (row) begins, s into the file: where the fade into it ends.
    pub begins: Vec<f64>,
    /// The fade into each row, s (0 for the first, and for a cut).
    pub fades: Vec<f64>,
    /// Where the last step ends, s into the file.
    pub end: f64,
}

/// One step per part. The fade into part k begins where the music begins to change (the start
/// of its blend zone, its start at a cut) and lasts the zone, at least `fade_min` seconds; the
/// step begins where the fade ends, so `step` reads the row the picture has arrived at and `mix`
/// the share of the next row. The last step ends at `end` (the file's length, or the parts' end
/// when that is later). `source` names the file in the first line.
pub fn steps(parts: &[Part], end: f64, fade_min: f64, source: &str) -> Result<Steps, String> {
    steps_with(parts, &[], end, fade_min, 0.0, source)
}

/// [`steps`] with a longer fade where the stage changes: `stages` gives each part's stage (as
/// the rows say), and the fade into a part whose stage differs from the one before lasts at
/// least `stage_fade` seconds.
pub fn steps_with(
    parts: &[Part],
    stages: &[usize],
    end: f64,
    fade_min: f64,
    stage_fade: f64,
    source: &str,
) -> Result<Steps, String> {
    if parts.is_empty() {
        return Err("the file has no sections".into());
    }
    let fade_min = if fade_min.is_finite() { fade_min.clamp(0.0, FADE_MIN_MAX) } else { FADE_MIN };
    let stage_fade = if stage_fade.is_finite() { stage_fade.clamp(0.0, FADE_MIN_MAX) } else { STAGE_FADE };
    let last_end = parts.iter().map(|p| p.end).filter(|t| t.is_finite()).fold(0.0, f64::max);
    let end = ms(if end.is_finite() { end.max(last_end) } else { last_end });
    let min_fade = ms(fade_min);
    // Where each step begins and the fade into it, in ms.
    let mut begins: Vec<i64> = Vec::with_capacity(parts.len());
    let mut fades: Vec<i64> = Vec::with_capacity(parts.len());
    for (k, p) in parts.iter().enumerate() {
        if k == 0 {
            begins.push(0);
            fades.push(0);
            continue;
        }
        let (z0, z1) = (ms(p.zone.0), ms(p.zone.1));
        let zone = z1 - z0 >= MIN_ZONE_MS;
        let from = if zone { z0 } else { ms(p.start) };
        let length = if zone { (z1 - z0).max(min_fade) } else { min_fade };
        let turns = matches!((stages.get(k - 1), stages.get(k)), (Some(a), Some(b)) if a != b);
        let length = if turns { length.max(ms(stage_fade)) } else { length };
        let previous = begins.last().copied().unwrap_or(0);
        // The fade lies inside the step before; a step lasts at least MIN_STEP_MS.
        let from = from.max(previous + MIN_STEP_MS);
        let begin = (from + length).max(previous + MIN_STEP_MS);
        begins.push(begin);
        fades.push(begin - from);
    }
    let end = end.max(begins.last().copied().unwrap_or(0) + MIN_STEP_MS);
    let write = |notes: bool| -> String {
        let mut text = String::new();
        if notes {
            text.push_str(&format!(
                "# One step per row of the arrangement of {source}, written by beats.arrange. Seconds\n\
                 # follow the file: clock is bound to the seconds of its chan.beat_grid.\n"
            ));
        }
        for (k, b) in begins.iter().enumerate() {
            let until = begins.get(k + 1).copied().unwrap_or(end);
            let seconds = (until - b) as f64 / 1000.0;
            let fade = fades.get(k + 1).copied().unwrap_or(0);
            let mut line = if fade > 0 {
                format!("{PIECE} {seconds:.3} fade {:.3}", fade as f64 / 1000.0)
            } else {
                format!("{PIECE} {seconds:.3}")
            };
            if notes && let Some(p) = parts.get(k) {
                line = format!("{line:<28} # row {k}: {}, from {:.2} s", p.role.name(), p.start);
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    };
    let mut text = write(true);
    if text.len() > MAX_TEXT {
        text = write(false);
    }
    if text.len() > MAX_TEXT {
        return Err(format!("{} steps do not fit into a steps param ({MAX_TEXT} bytes)", begins.len()));
    }
    let secs = |v: &[i64]| v.iter().map(|t| *t as f64 / 1000.0).collect();
    Ok(Steps { text, begins: secs(&begins), fades: secs(&fades), end: end as f64 / 1000.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence::Show;

    fn slots() -> Vec<Slot> {
        vec![Slot::new("floor", &["blocks", "mountains"]), Slot::new("sky", &["galaxy"])]
    }

    /// Parts of 30 s each with the given roles and changes, energy 0.6, cuts.
    fn parts(of: &[(Role, f64)]) -> Vec<Part> {
        of.iter()
            .enumerate()
            .map(|(i, &(role, change))| {
                let start = i as f64 * 30.0;
                Part {
                    start,
                    end: start + 30.0,
                    zone: (start, start),
                    role,
                    change,
                    energy: 0.6,
                    track_start: false,
                    seed: i as u32,
                }
            })
            .collect()
    }

    /// The presence of `blocks`, `mountains`, `galaxy` per row.
    fn presence(r: &[Row]) -> Vec<[f64; 3]> {
        r.iter().map(|r| [r.presence[0], r.presence[1], r.presence[2]]).collect()
    }

    #[test]
    fn the_first_section_shows_the_first_element_of_each_slot() {
        let r = rows(&parts(&[(Role::Main, 0.9)]), &slots(), 1);
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0]]);
        assert_eq!(r[0].warmth, 0.0);
        // Its change does not count: nothing came before.
        assert_eq!(r[0].change, 0.0);
        assert!((r[0].intensity - 0.6).abs() < 1e-12);
    }

    #[test]
    fn a_small_change_walks_the_colour_and_swaps_nothing() {
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.2), (Role::Main, 0.29)]), &slots(), 1);
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0]; 3]);
        assert!((r[1].warmth - 0.2 * WALK_STEP).abs() < 1e-12);
        assert!((r[2].warmth - 0.49 * WALK_STEP).abs() < 1e-12);
    }

    #[test]
    fn a_medium_change_swaps_one_slot_and_a_big_one_every_slot_with_a_double_step() {
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.5)]), &slots(), 1);
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]);
        assert!((r[1].walk - 0.5 * WALK_STEP).abs() < 1e-12);
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.8)]), &slots(), 1);
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]);
        assert!((r[1].walk - 0.8 * WALK_STEP * 2.0).abs() < 1e-12);
        // Two floor slots: a big change swaps both, a medium one only one.
        let two = vec![Slot::new("floor", &["a", "b"]), Slot::new("mid", &["c", "d", "e"])];
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 1.0), (Role::Main, 0.5)]), &two, 1);
        assert_eq!(r[1].presence, [0.0, 1.0, 0.0, 1.0, 0.0]);
        assert_eq!(r[2].presence, [1.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn slots_take_turns_and_a_slot_of_one_element_never_swaps() {
        let s = vec![
            Slot::new("floor", &["a", "b"]),
            Slot::new("mid", &["c", "d", "e"]),
            Slot::new("sky", &["g"]),
        ];
        let p = parts(&[
            (Role::Main, 0.0),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
        ]);
        let r = rows(&p, &s, 1);
        let on = |r: &Row| -> Vec<usize> {
            r.presence.iter().enumerate().filter(|(_, p)| **p > 0.0).map(|(i, _)| i).collect()
        };
        assert_eq!(on(&r[0]), [0, 2, 5]);
        assert_eq!(on(&r[1]), [1, 2, 5], "floor first");
        assert_eq!(on(&r[2]), [1, 3, 5], "then mid");
        assert_eq!(on(&r[3]), [0, 3, 5], "floor again");
        assert_eq!(on(&r[4]), [0, 4, 5]);
        // Only one-element slots: nothing can swap.
        let single = vec![Slot::new("sky", &["g"])];
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.5), (Role::Main, 1.0)]), &single, 1);
        assert!(r.iter().all(|r| r.presence == [1.0]));
    }

    #[test]
    fn roles_set_presence_and_intensity() {
        let p = parts(&[
            (Role::Intro, 0.0),
            (Role::Build, 0.1),
            (Role::Break, 0.1),
            (Role::Drop, 0.1),
            (Role::Main, 0.1),
            (Role::Outro, 0.1),
        ]);
        let r = rows(&p, &slots(), 1);
        assert_eq!(
            presence(&r),
            [
                [CALM_PRESENCE, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, BREAK_SKY],
                [1.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [CALM_PRESENCE, 0.0, 1.0],
            ]
        );
        let i: Vec<f64> = r.iter().map(|r| r.intensity).collect();
        assert_eq!(i, [CALM_INTENSITY, 0.6, 0.6, 1.0, 0.6, CALM_INTENSITY]);
    }

    #[test]
    fn the_mid_slot_follows_the_sky_and_other_slots_the_floor() {
        let s = vec![
            Slot::new("ceiling", &["c"]),
            Slot::new("mid", &["g"]),
            Slot::new("other", &["p"]),
            Slot::new("sky", &["s"]),
        ];
        let p = parts(&[(Role::Intro, 0.0), (Role::Break, 0.5), (Role::Drop, 0.9), (Role::Outro, 0.1)]);
        let r: Vec<Vec<f64>> = rows(&p, &s, 1).into_iter().map(|r| r.presence).collect();
        assert_eq!(
            r,
            [
                [CALM_PRESENCE, 1.0, CALM_PRESENCE, 1.0],
                [0.0, BREAK_SKY, 0.0, BREAK_SKY],
                [1.0, 1.0, 1.0, 1.0],
                [CALM_PRESENCE, 1.0, CALM_PRESENCE, 1.0],
            ]
        );
    }

    #[test]
    fn the_walk_never_goes_back_and_warmth_is_its_triangle() {
        let mut of = vec![(Role::Main, 0.0)];
        for k in 0..60 {
            of.push((Role::Main, [0.1, 0.5, 0.95, 0.0, 0.35][k % 5]));
        }
        let r = rows(&parts(&of), &slots(), 1);
        for w in r.windows(2) {
            assert!(w[1].walk >= w[0].walk, "{} after {}", w[1].walk, w[0].walk);
            assert!((0.0..=1.0).contains(&w[1].warmth));
            assert!((w[1].warmth - warmth_at(w[1].walk)).abs() < 1e-12);
        }
        // Far enough to turn back at 1 at least once.
        assert!(r[r.len() - 1].walk > 2.0, "{}", r[r.len() - 1].walk);
        assert_eq!(warmth_at(0.5), 0.5);
        assert_eq!(warmth_at(1.0), 1.0);
        assert!((warmth_at(1.25) - 0.75).abs() < 1e-12);
        assert_eq!(warmth_at(2.0), 0.0);
        assert_eq!(warmth_at(f64::NAN), 0.0);
        // Hostile input: changes out of range or no number.
        let r = rows(
            &parts(&[(Role::Main, 0.0), (Role::Main, f64::NAN), (Role::Main, -3.0), (Role::Main, 7.0)]),
            &slots(),
            1,
        );
        assert_eq!(r[1].walk, 0.0);
        assert_eq!(r[2].walk, 0.0);
        assert!((r[3].walk - 2.0 * WALK_STEP).abs() < 1e-12);
    }

    #[test]
    fn the_table_has_a_row_per_section_and_the_columns_named() {
        let p = parts(&[(Role::Intro, 0.0), (Role::Drop, 0.9)]);
        let t = table(&slots(), &rows(&p, &slots(), 1));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "start,end,role,change,blocks,mountains,galaxy,warmth,intensity,camera,stage,gaps"
        );
        assert_eq!(lines[1], "0,30,intro,0,0.4,0,1,0,0.3,4,0,0");
        assert_eq!(lines[2], "30,60,drop,0.9,0,1,1,0.45,1,2,0,0");
        assert_eq!(lines.len(), 3);
        assert!(check(&slots()).is_ok());
        let bad = |e: &[&str]| vec![Slot::new("floor", e)];
        assert!(check(&bad(&["a", "a"])).unwrap_err().contains("twice"));
        assert!(check(&bad(&["warmth"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["camera"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["stage"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["gaps"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["a b"])).unwrap_err().contains("column name"));
        assert!(check(&bad(&[])).unwrap_err().contains("no elements"));
        assert!(check(&[]).is_err());
    }

    #[test]
    fn post_elements_are_on_each_by_itself_off_where_calm_and_on_from_a_drop() {
        let s = vec![Slot::new("floor", &["a"]), Slot::new("post", &["lines", "glow"])];
        let p = parts(&[
            (Role::Intro, 0.0),
            (Role::Main, 0.1),
            (Role::Drop, 0.9),
            (Role::Main, 0.1),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
            (Role::Break, 0.5),
            (Role::Main, 0.1),
            (Role::Main, 0.8),
            (Role::Outro, 0.5),
        ]);
        let r: Vec<Vec<f64>> = rows(&p, &s, 1).into_iter().map(|r| r.presence).collect();
        // Off at the start and through a small change; both on at a drop, and they stay on.
        assert_eq!(r[0], [CALM_PRESENCE, 0.0, 0.0]);
        assert_eq!(r[1], [1.0, 0.0, 0.0]);
        assert_eq!(r[2], [1.0, 1.0, 1.0]);
        assert_eq!(r[3], [1.0, 1.0, 1.0]);
        // A medium change switches the next post element: the post slot is the only one that swaps.
        assert_eq!(r[4], [1.0, 0.0, 1.0]);
        assert_eq!(r[5], [1.0, 0.0, 0.0]);
        // A break turns them off; a big change switches every one of them.
        assert_eq!(r[6], [0.0, 0.0, 0.0]);
        assert_eq!(r[7], [1.0, 0.0, 0.0]);
        assert_eq!(r[8], [1.0, 1.0, 1.0]);
        assert_eq!(r[9], [CALM_PRESENCE, 0.0, 0.0]);
        // A ceiling slot follows the floor: empty in a break.
        let c = vec![Slot::new(CEILING, &["chase"])];
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Break, 0.5), (Role::Intro, 0.9)]), &c, 1);
        let r: Vec<f64> = r.iter().map(|r| r.presence[0]).collect();
        assert_eq!(r, [1.0, 0.0, CALM_PRESENCE]);
    }

    /// Five slots, three full sections, then a main section at `change` with `seed`.
    fn empty_case(change: f64, seed: u32) -> Row {
        let s = vec![
            Slot::new("ceiling", &["c"]),
            Slot::new("floor", &["a", "b"]),
            Slot::new("mid", &["g"]),
            Slot::new("post", &["p"]),
            Slot::new("sky", &["s"]),
        ];
        let mut p = parts(&[(Role::Drop, 0.0), (Role::Main, 0.1), (Role::Main, 0.1), (Role::Main, change)]);
        p[3].seed = seed;
        rows(&p, &s, 1).remove(3)
    }

    #[test]
    fn a_strong_boundary_may_empty_a_main_section_to_one_slot_as_the_seed_draws() {
        assert_eq!(draw(7, 3), draw(7, 3), "the same draw every time");
        assert!((0..1000).all(|k| (0.0..1.0).contains(&draw(k, 3))));
        let full = (0..10_000u32).find(|&k| draw(k, 3) >= EMPTY_CHANCE).unwrap();
        let seed = (0..10_000u32).find(|&k| draw(k, 3) < 0.3 * EMPTY_CHANCE).unwrap();
        let r = empty_case(1.0, seed);
        assert!(r.empty);
        // One content slot is left at its full presence; the post element is off as well.
        let on: Vec<usize> =
            r.presence.iter().enumerate().filter(|(_, p)| **p > 0.0).map(|(i, _)| i).collect();
        assert_eq!(on.len(), 1, "{:?}", r.presence);
        assert_ne!(on[0], 4, "the post element is never the one kept");
        assert_eq!(r.presence[on[0]], 1.0);
        assert_eq!(r.camera, CAMERA_MAIN);
        // The same section with a draw above the chance, or a weaker boundary: full.
        assert!(!empty_case(1.0, full).empty);
        assert!(!empty_case(CHANGE_BIG - 0.01, seed).empty);
        // Fewer full sections before: none.
        let s = vec![Slot::new("floor", &["a"])];
        let mut p = parts(&[(Role::Main, 0.0), (Role::Break, 0.1), (Role::Main, 0.1), (Role::Main, 1.0)]);
        p[3].seed = seed;
        assert!(!rows(&p, &s, 1)[3].empty, "a break two rows before resets the count");
        // Only a main section empties: a drop at the same draw stays full.
        let mut p = parts(&[(Role::Main, 0.0), (Role::Main, 0.1), (Role::Main, 0.1), (Role::Drop, 1.0)]);
        p[3].seed = seed;
        assert!(!rows(&p, &s, 1)[3].empty);
        // About EMPTY_CHANCE times change of the seeds empty a strong boundary.
        let share = (0..2000u32).filter(|&k| empty_case(0.8, k).empty).count() as f64 / 2000.0;
        assert!((share - 0.8 * EMPTY_CHANCE).abs() < 0.04, "{share}");
        // An empty row resets the count: the row after it does not empty again.
        let mut p = parts(&[
            (Role::Main, 0.0),
            (Role::Main, 0.1),
            (Role::Main, 0.1),
            (Role::Main, 1.0),
            (Role::Main, 1.0),
        ]);
        p[3].seed = seed;
        p[4].seed = seed;
        let r = rows(&p, &s, 1);
        assert!(r[3].empty && !r[4].empty);
    }

    #[test]
    fn the_camera_follows_the_role_and_each_track_takes_a_variant() {
        let mut p = parts(&[
            (Role::Intro, 0.0),
            (Role::Main, 0.2),
            (Role::Build, 0.3),
            (Role::Drop, 0.9),
            (Role::Break, 0.6),
            (Role::Outro, 0.3),
            (Role::Main, 0.9),
            (Role::Main, 0.1),
            (Role::Intro, 0.95),
        ]);
        p[0].track_start = true;
        p[6].track_start = true;
        p[8].track_start = true;
        let cams: Vec<usize> = rows(&p, &slots(), 1).iter().map(|r| r.camera).collect();
        let [a, b] = CAMERA_VARIANTS;
        assert_eq!(cams, [a, CAMERA_MAIN, CAMERA_BUILD, CAMERA_DROP, CAMERA_BREAK, a, b, CAMERA_MAIN, a]);
        assert!(cams.iter().all(|c| *c < CAMERA_PRESETS));
    }

    #[test]
    fn the_stage_changes_only_at_a_strong_track_start_and_not_twice_in_its_gap() {
        // A track start every 3 minutes (6 parts of 30 s); every fourth is weak.
        let mut of = Vec::new();
        for k in 0..60 {
            of.push((if k % 6 == 0 { Role::Intro } else { Role::Main }, 0.5));
        }
        let mut p = parts(&of);
        for k in (0..60).step_by(6) {
            p[k].track_start = true;
            p[k].change = if (k / 6) % 4 == 3 { 0.7 } else { 0.9 };
        }
        // A strong boundary inside a track (a drop) never switches.
        p[20].role = Role::Drop;
        p[20].change = 1.0;
        let r = rows(&p, &slots(), 3);
        let stages: Vec<usize> = r.iter().map(|r| r.stage).collect();
        let switches: Vec<usize> = (1..60).filter(|&k| stages[k] != stages[k - 1]).collect();
        // Track starts at 180 and 360 s (too soon after the file's start), 540 (weak), 720,
        // 900 and 1080 (too soon), 1260 (weak), 1440, 1620 (too soon).
        let at: Vec<f64> = switches.iter().map(|&k| p[k].start).collect();
        assert_eq!(at, [720.0, 1440.0], "{stages:?}");
        assert!(switches.iter().all(|&k| p[k].track_start && p[k].change >= STAGE_CHANGE));
        assert!(at.windows(2).all(|w| w[1] - w[0] >= STAGE_GAP));
        assert_eq!(stages[59], 2);
        assert!(stages.iter().all(|s| *s < 3));
        // One stage: always 0.
        assert!(rows(&p, &slots(), 1).iter().all(|r| r.stage == 0));
        // With every track start strong and far enough apart, the stage walks to the end of the
        // list and back, one neighbour at a time.
        for (k, part) in p.iter_mut().enumerate() {
            part.start = 100.0 * k as f64;
            if part.track_start {
                part.change = 0.9;
            }
        }
        for (n, walk) in [(2, vec![0, 1, 0, 1, 0, 1, 0, 1, 0, 1]), (3, vec![0, 1, 2, 1, 0, 1, 2, 1, 0, 1])] {
            let at: Vec<usize> = rows(&p, &slots(), n).iter().step_by(6).map(|r| r.stage).collect();
            assert_eq!(at, walk, "{n} stages");
        }
        assert!(rows(&p, &slots(), 0).iter().all(|r| r.stage == 0));
    }

    #[test]
    fn the_steps_fade_through_the_zone_or_fade_min_and_the_show_reads_the_row() {
        let mut p = parts(&[(Role::Main, 0.0), (Role::Main, 0.5), (Role::Main, 0.5)]);
        // Part 1 begins at a cut at 30 s, part 2 blends in from 56 to 64 s.
        p[2].zone = (56.0, 64.0);
        let s = steps(&p, 95.0, 2.0, "x.wav").unwrap();
        assert_eq!(s.begins, [0.0, 32.0, 64.0]);
        assert_eq!(s.fades, [0.0, 2.0, 8.0]);
        assert_eq!(s.end, 95.0);
        assert!(s.text.starts_with("# One step per row"), "{}", s.text);
        let show = Show::new(&s.text, PIECE, 0.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        assert!((show.length() - 95.0).abs() < 1e-9);
        let at = |t: f64| show.at(t);
        assert_eq!((at(10.0).step, at(10.0).mix), (0, 0.0));
        // At the cut the fade begins: still row 0, half way at 31 s.
        assert_eq!(at(30.0).step, 0);
        assert!((at(31.0).mix - 0.5).abs() < 1e-9, "{:?}", at(31.0));
        assert_eq!((at(32.0).step, at(32.0).mix), (1, 0.0));
        // Through the zone.
        assert_eq!(at(60.0).step, 1);
        assert!((at(60.0).mix - 0.5).abs() < 1e-9);
        assert_eq!(at(64.5).step, 2);
        // fade_min 0: a cut where the music cuts.
        let s = steps(&p, 95.0, 0.0, "x.wav").unwrap();
        assert_eq!(s.begins, [0.0, 30.0, 64.0]);
        assert!(s.text.lines().nth(3).unwrap().starts_with("row 34.000 fade 8.000"), "{}", s.text);
        // Hostile: sections closer than their fades, an end before them, fade_min no number.
        let mut close = parts(&[(Role::Main, 0.0), (Role::Main, 0.5), (Role::Main, 0.5)]);
        close[1].start = 0.01;
        close[2].start = 0.02;
        for (end, fade) in [(0.0, f64::NAN), (f64::NAN, 1e9), (-1.0, -1.0)] {
            let s = steps(&close, end, fade, "x").unwrap();
            let show = Show::new(&s.text, PIECE, 0.0);
            assert!(show.problems().is_empty(), "{:?}\n{}", show.problems(), s.text);
            assert!(s.begins.windows(2).all(|w| w[1] >= w[0] + 0.1), "{:?}", s.begins);
        }
        assert!(steps(&[], 10.0, 2.0, "x").is_err());
    }

    #[test]
    fn the_floor_breaks_off_in_a_build_and_a_break_and_nowhere_else() {
        let p = parts(&[
            (Role::Intro, 0.0),
            (Role::Main, 0.5),
            (Role::Build, 0.4),
            (Role::Drop, 0.9),
            (Role::Break, 0.8),
            (Role::Outro, 0.5),
        ]);
        let gaps: Vec<f64> = rows(&p, &slots(), 1).iter().map(|r| r.gaps).collect();
        assert_eq!(gaps, [0.0, 0.0, GAPS_BUILD, 0.0, GAPS_BREAK, 0.0]);
        // Other shares, clamped to 0..1; no number counts as 0.
        let own = Gaps { build: 0.5, brk: 7.0 };
        let gaps: Vec<f64> = rows_with(&p, &slots(), 1, Rules { gaps: own, ..Rules::default() })
            .iter()
            .map(|r| r.gaps)
            .collect();
        assert_eq!(gaps, [0.0, 0.0, 0.5, 0.0, 1.0, 0.0]);
        let none = Gaps { build: f64::NAN, brk: -1.0 };
        assert!(
            rows_with(&p, &slots(), 1, Rules { gaps: none, ..Rules::default() })
                .iter()
                .all(|r| r.gaps == 0.0)
        );
        let t = table(&slots(), &rows(&p, &slots(), 1));
        assert!(t.lines().nth(5).is_some_and(|l| l.ends_with(",0.6")), "{t}");
    }

    #[test]
    fn a_heavy_element_comes_on_as_often_as_its_weight_spread_out_and_the_same_every_time() {
        let names = ["glider", "a", "b", "c", "d", "e", "f", "g"];
        let mut slot = Slot::new("mid", &names);
        slot.weights = vec![3.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let mut of = vec![(Role::Main, 0.0)];
        of.extend(std::iter::repeat_n((Role::Main, 0.5), 99));
        let p = parts(&of);
        let on = |r: &[Row]| -> Vec<usize> {
            r.iter().map(|r| r.presence.iter().position(|x| *x > 0.0).unwrap_or(usize::MAX)).collect()
        };
        let picks = on(&rows(&p, std::slice::from_ref(&slot), 1));
        assert_eq!(picks, on(&rows(&p, std::slice::from_ref(&slot), 1)), "the same every time");
        assert_eq!(picks[0], 0, "the first element starts");
        // 100 rows, weights 3 of 10: the glider in 30, each other element in 10.
        let count = |k: usize| picks.iter().filter(|x| **x == k).count();
        assert_eq!(count(0), 30, "{picks:?}");
        for k in 1..names.len() {
            assert_eq!(count(k), 10, "{k}: {picks:?}");
        }
        // Every row swaps, and the glider's turns spread out: never more than 5 rows apart.
        assert!(picks.windows(2).all(|w| w[0] != w[1]), "{picks:?}");
        let at: Vec<usize> = picks.iter().enumerate().filter(|(_, x)| **x == 0).map(|(i, _)| i).collect();
        assert!(at.windows(2).all(|w| w[1] - w[0] <= 5), "{at:?}");
        // Equal weights, or weights that are no number, go through the elements in order.
        let mut plain = Slot::new("mid", &["a", "b", "c"]);
        for w in [vec![], vec![1.0, 1.0, 1.0], vec![f64::NAN, -2.0, 0.0]] {
            plain.weights = w;
            let r = rows(&p[..7], std::slice::from_ref(&plain), 1);
            assert_eq!(on(&r), [0, 1, 2, 0, 1, 2, 0]);
        }
        assert_eq!(Slot { weights: vec![1e9], ..Slot::new("x", &["a"]) }.weight(0), WEIGHT_MAX);
    }

    #[test]
    fn an_element_shows_only_on_its_stages_and_its_slot_moves_on_elsewhere() {
        // The sun only on stage 0, the planet everywhere, the bars only on stages 1 and 2.
        let mut sky = Slot::new("sky", &["sun", "planet", "bars"]);
        sky.stages = vec![vec![0], vec![], vec![1, 2]];
        // Four tracks, each starting with a strong change, far enough apart to switch the stage.
        let mut p = Vec::new();
        for i in 0..40 {
            let start = i as f64 * 60.0;
            let track_start = i % 10 == 0;
            p.push(Part {
                start,
                end: start + 60.0,
                zone: (start, start),
                role: Role::Main,
                change: if track_start { 0.9 } else { 0.4 },
                energy: 0.6,
                track_start,
                seed: i as u32,
            });
        }
        let r = rows(&p, std::slice::from_ref(&sky), 3);
        assert!(r.iter().any(|r| r.stage == 1) && r.iter().any(|r| r.stage == 2), "the stage walks");
        for row in &r {
            let on: Vec<usize> = (0..3).filter(|j| row.presence[*j] > 0.0).collect();
            assert_eq!(on.len(), 1, "one element on in every row: {row:?}");
            for j in on {
                assert!(sky.shows_in(j, row.stage), "{j} on stage {}: {row:?}", row.stage);
            }
        }
        assert!(r.iter().any(|r| r.presence[0] > 0.0), "the sun still comes on on stage 0");
        // A slot whose only element does not show on the stage stays dark there.
        let mut alone = Slot::new("sky", &["sun"]);
        alone.stages = vec![vec![0]];
        for row in rows(&p, std::slice::from_ref(&alone), 3) {
            assert_eq!(row.presence[0] > 0.0, row.stage == 0, "{row:?}");
        }
        // No lists: the same table as before.
        let plain = Slot::new("sky", &["sun", "planet", "bars"]);
        let mut every = plain.clone();
        every.stages = vec![vec![], vec![], vec![]];
        assert_eq!(rows(&p, std::slice::from_ref(&plain), 3), rows(&p, std::slice::from_ref(&every), 3));
    }

    /// Five slots taking turns at medium boundaries, the floor (four elements) on stage 0 only;
    /// a track start every 10 parts of 60 s switches the stage.
    fn dwell_case() -> (Vec<Part>, Vec<Slot>) {
        let mut floor = Slot::new("floor", &["a", "b", "c", "d"]);
        floor.stages = vec![vec![0]; 4];
        let slots = vec![
            floor,
            Slot::new("ceiling", &["e", "f"]),
            Slot::new("mid", &["g", "h"]),
            Slot::new("sky", &["i", "j"]),
            Slot::new("wall", &["k", "l"]),
        ];
        let mut p = Vec::new();
        for i in 0..30 {
            let start = i as f64 * 60.0;
            let track_start = i % 10 == 0;
            p.push(Part {
                start,
                end: start + 60.0,
                zone: (start, start),
                role: Role::Main,
                change: if track_start { 0.9 } else { 0.4 },
                energy: 0.6,
                track_start,
                seed: 7,
            });
        }
        (p, slots)
    }

    /// The floor element on per row, or none.
    fn floor_on(r: &[Row]) -> Vec<Option<usize>> {
        r.iter().map(|r| (0..4).find(|j| r.presence[*j] > 0.0)).collect()
    }

    /// The longest run of rows one floor element stays on.
    fn longest_run(on: &[Option<usize>]) -> usize {
        let mut best = 0;
        let mut run = 0;
        for (k, x) in on.iter().enumerate() {
            run = if x.is_some() && k > 0 && on[k - 1] == *x { run + 1 } else { 1 };
            best = best.max(run);
        }
        best
    }

    #[test]
    fn an_element_on_for_its_dwell_gives_way_and_a_hidden_slot_counts_on() {
        let (p, slots) = dwell_case();
        let r = rows(&p, &slots, 2);
        let stages: Vec<usize> = r.iter().map(|r| r.stage).collect();
        assert_eq!(&stages[..], &[[0; 10], [1; 10], [0; 10]].concat()[..], "{stages:?}");
        let on = floor_on(&r);
        // Hidden on stage 1, shown on stage 0.
        assert!(on[10..20].iter().all(Option::is_none), "{on:?}");
        assert!(on[..10].iter().chain(&on[20..]).all(Option::is_some), "{on:?}");
        // With five slots the floor's turn comes every fifth medium boundary; the dwell swaps
        // it after three sections.
        assert!(longest_run(&on[..10]) <= DWELL && longest_run(&on[20..]) <= DWELL, "{on:?}");
        let off = rows_with(&p, &slots, 2, Rules { dwell: 0, ..Rules::default() });
        assert!(longest_run(&floor_on(&off)[..10]) > DWELL, "{:?}", floor_on(&off));
        // Every floor element gets rows with the dwell; without it only some do.
        let shown = |on: &[Option<usize>]| (0..4).filter(|j| on.contains(&Some(*j))).count();
        assert_eq!(shown(&on), 4, "{on:?}");
        // The slots under the dwell still swap on their turns, so every boundary from 0.3
        // swaps at least one slot where the floor shows (elsewhere the floor's turn passes).
        for k in (1..10).chain(20..30) {
            assert_ne!(r[k].presence, r[k - 1].presence, "row {k}");
        }
        // Back on stage 0 the floor shows another element than the one it left with.
        assert_ne!(on[20], on[9], "{on:?}");
        assert_eq!(rows(&p, &slots, 2), r, "the same every time");
    }

    #[test]
    fn the_fade_into_a_row_on_another_stage_lasts_at_least_the_stage_fade() {
        let mut p = parts(&[(Role::Main, 0.0), (Role::Main, 0.5), (Role::Main, 0.9), (Role::Main, 0.9)]);
        // Part 3 blends in from 84 to 94 s, longer than the stage fade.
        p[3].zone = (84.0, 94.0);
        let s = steps_with(&p, &[0, 0, 1, 0], 125.0, 2.0, 4.0, "x").unwrap();
        assert_eq!(s.fades, [0.0, 2.0, 4.0, 10.0]);
        assert_eq!(s.begins, [0.0, 32.0, 64.0, 94.0]);
        let show = Show::new(&s.text, PIECE, 0.0);
        assert!(show.problems().is_empty(), "{:?}", show.problems());
        assert!((show.at(62.0).mix - 0.5).abs() < 1e-9, "{:?}", show.at(62.0));
        // Without stages, or a stage fade of 0, the fades of steps.
        assert_eq!(steps_with(&p, &[], 125.0, 2.0, 4.0, "x").unwrap(), steps(&p, 125.0, 2.0, "x").unwrap());
        assert_eq!(steps_with(&p, &[0, 0, 1, 0], 125.0, 2.0, 0.0, "x").unwrap().fades, [0.0, 2.0, 2.0, 10.0]);
        // Hostile: no number takes the default, too long the longest.
        assert_eq!(steps_with(&p, &[0, 0, 1, 1], 125.0, 2.0, f64::NAN, "x").unwrap().fades[2], STAGE_FADE);
        assert_eq!(steps_with(&p, &[0, 0, 1, 1], 300.0, 2.0, 1e9, "x").unwrap().fades[2], FADE_MIN_MAX);
    }

    /// The opening brings the sky, the mid slot, the floor and the ceiling in one after another,
    /// each on a bar start and fading in over a bar; then the arrangement goes on as before.
    #[test]
    fn the_opening_brings_the_slots_in_one_after_another() {
        let slots = vec![
            Slot::new("ceiling", &["lights"]),
            Slot::new("floor", &["blocks", "mountains"]),
            Slot::new("mid", &["glider"]),
            Slot::new("post", &["scan"]),
            Slot::new("sky", &["galaxy"]),
        ];
        let p = parts(&[(Role::Intro, 0.0), (Role::Drop, 0.8)]);
        let bars: Vec<f64> = (0..30).map(|i| f64::from(i) * 2.0).collect();
        let (split, n) = opening(&p, &slots, &bars);
        assert_eq!(n, 4);
        assert_eq!(split.len(), 5);
        assert_eq!(split[0].start, 0.0);
        assert_eq!(split[3].end, 30.0);
        assert_eq!(split[4], p[1]);
        for k in 1..4 {
            assert!(bars.contains(&split[k].start), "{k} starts on a bar: {:?}", split[k]);
            assert_eq!(split[k].zone, (split[k].start, split[k].start + 2.0));
            assert_eq!(split[k].change, 0.0);
            assert_eq!(split[k - 1].end, split[k].start);
        }
        let r = rows_with(&split, &slots, 1, Rules { opening: n, ..Rules::default() });
        let cols = columns(&slots);
        let on = |row: &Row, name: &str| {
            cols.iter().position(|c| c == name).is_some_and(|i| row.presence[i] > 0.0)
        };
        let want = [
            ["galaxy"].as_slice(),
            &["galaxy", "glider"],
            &["galaxy", "glider", "blocks"],
            &["galaxy", "glider", "blocks", "lights"],
        ];
        for (row, names) in r.iter().zip(want) {
            for c in &cols {
                assert_eq!(on(row, c), names.contains(&c.as_str()), "{c} in {row:?}");
            }
        }
        // The drop after it: everything on, the floor swapped once as without the opening.
        let plain = rows(&p, &slots, 1);
        assert_eq!(r[4].presence, plain[1].presence);
        // A short first section brings several slots at once; a very short one stays whole.
        let short = parts(&[(Role::Intro, 0.0)]);
        let few: Vec<f64> = vec![0.0, 10.0, 20.0, 25.0];
        assert_eq!(opening(&short, &slots, &few).1, 2);
        assert_eq!(opening(&short, &slots, &[0.0, 10.0]).1, 1);
        assert_eq!(opening(&[], &slots, &bars).1, 0);
    }
}
