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
//! - **Stand-ins:** an element may stand in for another one, of its own slot or another, on
//!   its stages. It takes no turns of its own: on its stages it shows wherever the other one
//!   would, with the same presence, and the other one stays at 0 there. Elsewhere it stays at 0.
//!   The swaps and turns of every slot stay what they were without it, so adding a stand-in
//!   changes the table only in the rows where it takes over.
//! - **Companions:** an element may come along with another one (the traps with the glider). It
//!   takes no turns either. On its stages it shows in a build, or in a main section of energy
//!   below [`COMPANION_ENERGY`], where the other one is the only element of its slot on, with
//!   the other one's presence; not in the rows on either side of a change of stage. In those rows
//!   the glider flies through it ([`FLIGHT_THREAD`]). Every other cell stays as without it.
//! - **Extras:** an element may come now and then by itself, in lively rows (a drop, a build, a
//!   main section of energy from [`LIVELY_ENERGY`]), in calm ones (an intro, an outro, a break)
//!   or in steady ones (a main section of energy below [`LIVELY_ENERGY`]) ([`Extra`]). It takes no turns either. On its stages it shows in every second stretch
//!   of such rows, the first one included, with its slot's presence for the row; a stretch is the
//!   rows of that kind that follow each other, counted per stage. Not in the rows on either side
//!   of a change of stage. Every other cell stays as without it.
//! - **Base:** a slot may hold one base element. It takes no turns: it stays on under whichever
//!   element has the slot's turn, with the presence an element of the slot would have on (calm
//!   in an intro or outro, off where a break or the opening leaves the slot empty), on its stages.
//!   An empty section keeps no base, since it keeps one element only. The swaps and turns of
//!   every slot stay what they were without it, and no companion counts it as on.
//! - **Apart:** a pair of elements of different slots may be kept from sharing a row. Where both
//!   would be on, the first named gives way for that row: its slot shows the element its next swap
//!   would bring, with the same presence. The slot's turns stay as they are, so every other cell
//!   stays as without the pair.
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
//! - **Flight:** each row names how a mid element that flies (the glider) moves: along the
//!   lane's turns ([`FLIGHT_SLALOM`]), straight down the lane's middle ([`FLIGHT_STRAIGHT`]),
//!   along the turns with a roll on a downbeat ([`FLIGHT_ROLL`]), or straight with a lean on each
//!   downbeat ([`FLIGHT_BANK`]). Straight in a break, rolls on a drop, the lean in a build, and a
//!   variant per track in an intro, an outro and a main section ([`FLIGHT_VARIANTS`]). Where a
//!   companion is on, the glider follows its corridor ([`FLIGHT_THREAD`]).
//! - **Pattern:** each row names the pattern the lights of the room show (a `room_lights` in the
//!   library): stripes rising up the columns ([`PATTERN_STRIPES`]), every column flashing on the
//!   beat ([`PATTERN_PULSE`]), or one column after the other ([`PATTERN_CHASE`]). Stripes where
//!   it is calm (an intro, an outro, a break), the chase in a build, the pulse on a drop, and a
//!   variant per track in a main section ([`PATTERN_VARIANTS`]).
//! - **Stage:** with several stages, the row says which one shows. It changes only where a track
//!   starts with a `change` of at least [`STAGE_CHANGE`], or at the drop after a found break
//!   ([`with_breaks`]), and at most once per [`STAGE_GAP`] (the caller may give another gap).
//!   It walks to the next stage and back again (0, 1, 2, 1, 0, 1, ...), so every change goes to a
//!   neighbour in the list: a blending switch fades between the two and never through a third.
//! - **Opening:** the file's first section brings its slots in one after another, at bar starts:
//!   first the sky, then the mid slot, then the floor and the other slots, the ceiling last. It
//!   is split into one row per step ([`opening`]), at least [`OPENING_BARS`] bars each, and each
//!   step fades in over a bar. The steps count as one section for the dwell.
//! - **Gaps:** the row says how much of the floor breaks off into gaps: [`GAPS_BUILD`] in a
//!   build, [`GAPS_BREAK`] in a break, none elsewhere. The caller may give other shares
//!   ([`Gaps`]).
//! - **See-through:** the row says how far a stage with walls (the tunnel) opens them, so the sky
//!   shows through. Every second stretch of breaks and builds on each stage opens to
//!   [`SEE_THROUGH`], the first one included; a stretch is the rows of breaks and builds that
//!   follow each other. Every other row keeps the walls closed, and so do the rows on either side
//!   of a change of stage, so the walls never open while one stage fades into the next.
//!
//! - **A single song:** the rules above were tuned on long mixes, whose sections are short against
//!   the file. For one song the caller may cut the sections finer. [`bass_gone`] finds the
//!   stretches without bass, [`with_breaks`] makes each a section of role break with a drop after
//!   it, and [`split`] cuts long sections at bar starts. All three are off unless asked for.
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
/// The glider follows the lane's turns and leans into them.
pub const FLIGHT_SLALOM: usize = 0;
/// The glider flies straight down the lane's middle, level.
pub const FLIGHT_STRAIGHT: usize = 1;
/// The glider follows the turns and rolls once on a downbeat every few bars.
pub const FLIGHT_ROLL: usize = 2;
/// The glider flies straight and leans to one side and back on each downbeat.
pub const FLIGHT_BANK: usize = 3;
/// The glider follows the free corridor through a companion's beams (`mid_traps`).
pub const FLIGHT_THREAD: usize = 4;
/// The flights the tracks take in turn in an intro, an outro and a main section.
pub const FLIGHT_VARIANTS: [usize; 3] = [FLIGHT_SLALOM, FLIGHT_STRAIGHT, FLIGHT_BANK];
/// The room's columns show stripes that rise slowly: calm sections.
pub const PATTERN_STRIPES: usize = 0;
/// Every column flashes on the beat: a drop.
pub const PATTERN_PULSE: usize = 1;
/// The light runs from one column to the next on each beat: a build.
pub const PATTERN_CHASE: usize = 2;
/// The patterns the tracks take in turn in a main section.
pub const PATTERN_VARIANTS: [usize; 2] = [PATTERN_CHASE, PATTERN_PULSE];
/// The least `change` at a track start that may switch the stage.
pub const STAGE_CHANGE: f64 = 0.8;
/// The least time between two switches of the stage, in seconds; the file's start counts as one.
pub const STAGE_GAP: f64 = 480.0;
/// The `change` of a boundary [`split`] adds: the least that swaps a slot, so one slot swaps at
/// each such boundary and the colour takes a small step. The music hardly changes there, and the
/// measured novelty would stay below [`CHANGE_SMALL`] and swap nothing.
pub const SPLIT_CHANGE: f64 = CHANGE_SMALL;
/// The longest `split_bars` taken.
pub const SPLIT_BARS_MAX: usize = 1000;
/// A bar counts as without bass this many dB below the file's loud bass level ([`bass_gone`]).
pub const BASS_GONE_DB: f64 = 10.0;
/// A file whose loud bass level stays below this, in dBFS, has no bass to lose.
pub const BASS_PRESENT_DB: f64 = -60.0;
/// The fewest bars without bass that make a break. A shorter dip is a fill.
pub const BREAK_MIN_BARS: usize = 4;
/// A found break begins at a section start this many bars away, so no row of a bar or two
/// stays before it, and a section start this near after its end goes.
pub const BREAK_SNAP_BARS: usize = 2;
/// The least `change` into a found break: the floor empties by role, and one slot swaps.
pub const BREAK_CHANGE: f64 = 0.5;
/// The `change` of the drop after a found break: every slot swaps and the stage may change.
pub const DROP_CHANGE: f64 = 1.0;
/// The fewest bars a step of the opening lasts ([`opening`]).
pub const OPENING_BARS: usize = 2;
/// The share of the floor in gaps during a build.
pub const GAPS_BUILD: f64 = 0.3;
/// The share of the floor in gaps during a break.
pub const GAPS_BREAK: f64 = 0.6;
/// How far the walls open in a row that sees through them: wholly.
pub const SEE_THROUGH: f64 = 1.0;
/// A companion comes along in a main section only below this energy: a calmer stretch, not the
/// height of a track.
pub const COMPANION_ENERGY: f64 = 0.8;
/// A main section counts as lively for an extra from this energy on: the height of a track.
pub const LIVELY_ENERGY: f64 = 0.8;

/// The stretches of one kind of extra's rows: how many each stage has shown, whether the one
/// going on brings the extras, and whether the row before was of the kind.
struct Run {
    kind: Extra,
    stretches: Vec<usize>,
    open: bool,
    before: bool,
}

/// When an extra comes ([`Slot::extra`]): in lively rows, in calm ones or in steady ones. No row
/// is of two kinds, so extras of different kinds never show together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extra {
    /// A drop, a build, or a main section of energy from [`LIVELY_ENERGY`].
    Lively,
    /// An intro, an outro or a break.
    Calm,
    /// A main section of energy below [`LIVELY_ENERGY`].
    Steady,
}

impl Extra {
    /// The kind by its name in the command: `lively`, `calm` or `steady`.
    pub fn named(name: &str) -> Option<Extra> {
        match name {
            "lively" => Some(Extra::Lively),
            "calm" => Some(Extra::Calm),
            "steady" => Some(Extra::Steady),
            _ => None,
        }
    }

    /// Whether a section of `role` and `energy` is one of this kind.
    pub fn fits(self, role: Role, energy: f64) -> bool {
        match self {
            Extra::Lively => match role {
                Role::Drop | Role::Build => true,
                Role::Main => energy.is_finite() && energy >= LIVELY_ENERGY,
                _ => false,
            },
            Extra::Calm => matches!(role, Role::Intro | Role::Outro | Role::Break),
            Extra::Steady => role == Role::Main && energy.is_finite() && energy < LIVELY_ENERGY,
        }
    }
}

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

/// What the caller may set of the rules: the floor's gaps, the dwell, the opening and the pairs
/// kept apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    pub gaps: Gaps,
    /// The sections an element stays on before it gives way at the next boundary from
    /// [`CHANGE_SMALL`]; 0 turns the rule off.
    pub dwell: usize,
    /// How many of the first parts are the steps of the opening ([`opening`]): their rows bring
    /// the slots in one after another. 0 or 1: no opening.
    pub opening: usize,
    /// Pairs of columns that never share a row; the first of a pair gives way ([`check_apart`]).
    pub apart: Vec<(String, String)>,
    /// The least time between two switches of the stage, in seconds.
    pub stage_gap: f64,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules { gaps: Gaps::default(), dwell: DWELL, opening: 0, apart: Vec::new(), stage_gap: STAGE_GAP }
    }
}

/// The stretches without bass in a file, as (start, end) in seconds, each from one bar start to
/// another. `edges` holds the bar starts and then the file's end, `bass` the mean low band of each
/// bar in dBFS, so one number less than `edges`.
///
/// A bar counts as without bass [`BASS_GONE_DB`] below the upper quartile of all bars. One bar with
/// bass between two without does not end a stretch. A stretch needs [`BREAK_MIN_BARS`] bars, and
/// bass before and after it, so a quiet start and a fading end are no breaks.
pub fn bass_gone(edges: &[f64], bass: &[f64]) -> Vec<(f64, f64)> {
    let mut sorted: Vec<f64> = bass.iter().copied().filter(|x| x.is_finite()).collect();
    sorted.sort_by(f64::total_cmp);
    let Some(&loud) = sorted.get(sorted.len() * 3 / 4) else { return Vec::new() };
    if loud < BASS_PRESENT_DB {
        return Vec::new();
    }
    let without = |i: usize| bass.get(i).is_some_and(|x| !x.is_finite() || *x < loud - BASS_GONE_DB);
    let gone = |i: usize| without(i) || (i > 0 && without(i - 1) && without(i + 1));
    let mut found = Vec::new();
    let mut i = 0;
    while i < bass.len() {
        if !gone(i) {
            i += 1;
            continue;
        }
        let first = i;
        while i < bass.len() && gone(i) {
            i += 1;
        }
        if first > 0
            && i < bass.len()
            && i - first >= BREAK_MIN_BARS
            && let (Some(from), Some(to)) = (edges.get(first), edges.get(i))
        {
            found.push((*from, *to));
        }
    }
    found
}

/// The blend zone of a boundary at `at` that the music gives none for: the bar before it, so a
/// fade of the usual length has arrived when the bar begins. A cut where no bar lies before it.
fn arrive(bars: &[f64], at: f64) -> (f64, f64) {
    let k = bars.partition_point(|b| *b < at);
    match k.checked_sub(1).and_then(|p| bars.get(p)) {
        Some(b) if b.is_finite() => (*b, at),
        _ => (at, at),
    }
}

/// `parts` with each stretch of `found` ([`bass_gone`]) as a section of role break and a drop
/// after it. The drop runs to the end of the section it falls in. The parts the stretch overlaps
/// lose what it covers. `energy` gives the mean energy from one moment to another.
///
/// The section finder takes a deep break for the start of a new track. A track start at or inside
/// a found break is therefore dropped, and an outro right before it becomes a main section.
pub fn with_breaks(
    parts: &[Part],
    found: &[(f64, f64)],
    bars: &[f64],
    energy: &dyn Fn(f64, f64) -> f64,
) -> Vec<Part> {
    let mut out = parts.to_vec();
    for &(from, to) in found {
        out = one_break(&out, from, to, bars, energy);
    }
    out
}

fn one_break(
    parts: &[Part],
    from: f64,
    to: f64,
    bars: &[f64],
    energy: &dyn Fn(f64, f64) -> f64,
) -> Vec<Part> {
    // The length of BREAK_SNAP_BARS bars around here, by the bar the stretch begins with.
    let bar = bars.iter().copied().find(|b| *b > from).map_or(0.0, |b| b - from);
    let reach = bar * BREAK_SNAP_BARS as f64;
    let near = |s: f64| (s - from).abs() <= reach && s < to;
    let from = parts
        .iter()
        .map(|p| p.start)
        .filter(|s| near(*s))
        .min_by(|a, b| (a - from).abs().total_cmp(&(b - from).abs()))
        .unwrap_or(from);
    let Some(first) = parts.iter().position(|p| p.end > from) else { return parts.to_vec() };
    let Some(mut last) = parts.iter().position(|p| p.end > to) else { return parts.to_vec() };
    if parts.get(last).is_some_and(|p| p.end - to <= reach) && last + 1 < parts.len() {
        last += 1;
    }
    let (Some(begins), Some(goes_on)) = (parts.get(first), parts.get(last)) else {
        return parts.to_vec();
    };
    let mut out: Vec<Part> = parts.iter().take(first).copied().collect();
    if begins.start < from {
        out.push(Part { end: from, ..*begins });
    }
    let false_track = parts
        .iter()
        .skip(first)
        .take(last + 1 - first)
        .any(|p| p.track_start && p.start >= from && p.start <= to);
    if false_track
        && let Some(before) = out.last_mut()
        && before.role == Role::Outro
    {
        before.role = Role::Main;
    }
    let entered = if begins.start == from { unit(begins.change) } else { 0.0 };
    out.push(Part {
        start: from,
        end: to,
        zone: arrive(bars, from),
        role: Role::Break,
        change: entered.max(BREAK_CHANGE),
        energy: energy(from, to),
        track_start: false,
        seed: begins.seed,
        after_break: false,
        whole: false,
    });
    out.push(Part {
        start: to,
        end: goes_on.end,
        zone: arrive(bars, to),
        role: Role::Drop,
        change: DROP_CHANGE,
        energy: energy(to, goes_on.end),
        track_start: false,
        seed: goes_on.seed,
        after_break: true,
        whole: false,
    });
    out.extend(parts.iter().skip(last + 1).copied());
    out
}

/// `parts` with each long section cut at every `every`-th of `bars`, counted from its start. A
/// rest shorter than half of `every` stays with the piece before it. A break stays whole. A piece
/// after the first keeps the section's role, but a drop's becomes main, since a drop is the moment
/// a section begins with. Each new boundary has a `change` of [`SPLIT_CHANGE`], and each piece
/// the mean energy `energy` gives for it. Every boundary between sections is marked
/// [`Part::whole`], so it shows more than a cut inside a section. The music's blend into a section begins at the earliest half
/// way through the row before it, so that row still shows.
pub fn split(parts: &[Part], bars: &[f64], every: usize, energy: &dyn Fn(f64, f64) -> f64) -> Vec<Part> {
    if every == 0 {
        return parts.to_vec();
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let halfway = out.last().map_or(part.zone.0, |p: &Part| (p.start + p.end) / 2.0);
        let whole = !out.is_empty();
        let part = &Part { zone: (part.zone.0.max(halfway), part.zone.1), whole, ..*part };
        let inside: Vec<f64> =
            bars.iter().copied().filter(|b| b.is_finite() && *b > part.start && *b < part.end).collect();
        // The cuts: the bars after `every`, 2 `every`, ... from the start, while enough stay after.
        let count = inside.len() + 1;
        let keep = every.div_ceil(2);
        let cuts: Vec<f64> = (1..)
            .map(|k| k * every)
            .take_while(|at| at + keep <= count)
            .filter_map(|at| inside.get(at - 1).copied())
            .collect();
        if part.role == Role::Break || cuts.is_empty() {
            out.push(*part);
            continue;
        }
        let mut start = part.start;
        for end in cuts.iter().copied().chain([part.end]) {
            let piece = if start == part.start {
                *part
            } else {
                Part {
                    zone: arrive(bars, start),
                    role: if part.role == Role::Drop { Role::Main } else { part.role },
                    change: SPLIT_CHANGE,
                    track_start: false,
                    after_break: false,
                    whole: false,
                    ..*part
                }
            };
            out.push(Part { start, end, energy: energy(start, end), ..piece });
            start = end;
        }
    }
    out
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
pub const TRAILING: [&str; 8] =
    ["warmth", "intensity", "camera", "stage", "gaps", "flight", "pattern", "see_through"];
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
    /// It is the drop after a found break ([`with_breaks`]): the stage may change here.
    pub after_break: bool,
    /// Every slot swaps where it begins, whatever `change` says ([`split`]). The other rules that
    /// read `change` still go by the measured value, so such a boundary does not empty a row.
    pub whole: bool,
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
    /// The element each one stands in for, by column name, or none: one entry per element, or
    /// empty for none at all. A stand-in takes no turns; on its stages it shows where the other
    /// one would, and the other one stays at 0 there.
    pub instead: Vec<Option<String>>,
    /// The element each one comes along with, by column name, or none: one entry per element, or
    /// empty for none at all. A companion takes no turns; see the module's notes.
    pub with: Vec<Option<String>>,
    /// When each element comes by itself as an extra, or none: one entry per element, or empty
    /// for none at all. An extra takes no turns; see the module's notes.
    pub extra: Vec<Option<Extra>>,
    /// Whether each element is the slot's base: one entry per element, or empty for none at all.
    /// At most one per slot. A base takes no turns; see the module's notes.
    pub base: Vec<bool>,
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
            instead: Vec::new(),
            with: Vec::new(),
            extra: Vec::new(),
            base: Vec::new(),
        }
    }

    /// Whether element `i` is the slot's base.
    pub fn is_base(&self, i: usize) -> bool {
        self.base.get(i).copied().unwrap_or(false)
    }

    /// The element that element `i` stands in for, if it stands in for one.
    pub fn stands_in_for(&self, i: usize) -> Option<&str> {
        self.instead.get(i).and_then(|x| x.as_deref())
    }

    /// The element that element `i` comes along with, if it is a companion.
    pub fn comes_with(&self, i: usize) -> Option<&str> {
        self.with.get(i).and_then(|x| x.as_deref())
    }

    /// When element `i` comes by itself, if it is an extra.
    pub fn extra_of(&self, i: usize) -> Option<Extra> {
        self.extra.get(i).copied().flatten()
    }

    /// Whether element `i` takes no turns of its own: a stand-in, a companion, an extra or the base.
    fn follows(&self, i: usize) -> bool {
        self.stands_in_for(i).is_some()
            || self.comes_with(i).is_some()
            || self.extra_of(i).is_some()
            || self.is_base(i)
    }

    /// Whether element `i` may come on by a swap on stage `stage`: it shows there and is no
    /// stand-in or companion.
    fn takes_turns(&self, i: usize, stage: usize) -> bool {
        self.shows_in(i, stage) && !self.follows(i)
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
    /// How the glider flies: [`FLIGHT_SLALOM`] and the others.
    pub flight: usize,
    /// What the room's lights show: [`PATTERN_STRIPES`] and the others.
    pub pattern: usize,
    /// How far a stage with walls opens them, 0 to 1 ([`SEE_THROUGH`]).
    pub see_through: f64,
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
/// one of the table's own columns, no element at all, a stand-in or companion for no element,
/// for itself, for another stand-in, companion, extra or base, or in or for the post slot, an extra
/// in the post slot or one that also stands in for or comes with another, or a base that is also
/// one of those, one in the post slot, or a second base in a slot.
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
    for slot in slots {
        let bases: Vec<&String> =
            slot.elements.iter().enumerate().filter(|(i, _)| slot.is_base(*i)).map(|(_, c)| c).collect();
        if let [first, second, ..] = bases[..] {
            return Err(format!(
                "slot `{}` has two bases, `{first}` and `{second}`: a slot takes one base at most",
                slot.name
            ));
        }
        for (i, c) in slot.elements.iter().enumerate() {
            if slot.is_base(i) {
                if slot.name == POST {
                    return Err(format!("`{c}` is in the post slot, whose elements cannot be a base"));
                }
                if slot.stands_in_for(i).is_some()
                    || slot.comes_with(i).is_some()
                    || slot.extra_of(i).is_some()
                {
                    return Err(format!(
                        "`{c}` is the base of slot `{}`: a base cannot also stand in for or come with another, or be an extra",
                        slot.name
                    ));
                }
            }
            if slot.stands_in_for(i).is_some() && slot.comes_with(i).is_some() {
                return Err(format!(
                    "`{c}` either stands in for another element or comes with one, not both"
                ));
            }
            if slot.extra_of(i).is_some() {
                if slot.name == POST {
                    return Err(format!("`{c}` is in the post slot, whose elements cannot be extras"));
                }
                if slot.stands_in_for(i).is_some() || slot.comes_with(i).is_some() {
                    return Err(format!(
                        "`{c}` is an extra: it comes by itself and cannot stand in for or come with another"
                    ));
                }
            }
            for (other, does) in [(slot.stands_in_for(i), "stand in for"), (slot.comes_with(i), "come with")]
            {
                let Some(other) = other else { continue };
                if slot.name == POST {
                    return Err(format!("`{c}` is in the post slot, whose elements cannot {does} another"));
                }
                if other == c {
                    return Err(format!("`{c}` cannot {does} itself"));
                }
                let Some(of) = slots.iter().find(|s| s.elements.iter().any(|e| e == other)) else {
                    return Err(format!("`{c}` cannot {does} `{other}`, which is no element of a slot"));
                };
                if of.name == POST {
                    return Err(format!("`{c}` cannot {does} `{other}` of the post slot"));
                }
                let j = of.elements.iter().position(|e| e == other).unwrap_or(0);
                if of.follows(j) {
                    return Err(format!(
                        "`{c}` cannot {does} `{other}`, which stands in for or comes with another itself, or is an extra or a base"
                    ));
                }
            }
        }
        if slot.name != POST && (0..slot.elements.len()).all(|i| slot.follows(i)) {
            return Err(format!(
                "slot `{}` has only stand-ins, companions, extras and a base: it needs an element of its own",
                slot.name
            ));
        }
    }
    Ok(())
}

/// Checks pairs kept apart: two elements of different slots, none of them in the post slot, no
/// stand-in and no companion. The first named takes turns, so it has a next element to give way to.
pub fn check_apart(slots: &[Slot], pairs: &[(String, String)]) -> Result<(), String> {
    let find = |c: &str| -> Result<(usize, &Slot, usize), String> {
        slots
            .iter()
            .enumerate()
            .find_map(|(s, slot)| slot.elements.iter().position(|e| e == c).map(|j| (s, slot, j)))
            .ok_or_else(|| format!("apart names `{c}`, which is no element of a slot"))
    };
    for (first, second) in pairs {
        let (s1, slot1, j1) = find(first)?;
        let (s2, slot2, j2) = find(second)?;
        if s1 == s2 {
            return Err(format!(
                "apart names `{first}` and `{second}` of one slot: a slot shows one element at a time anyway"
            ));
        }
        for (c, slot, j) in [(first, slot1, j1), (second, slot2, j2)] {
            if slot.name == POST {
                return Err(format!("apart cannot name `{c}` of the post slot"));
            }
            if slot.stands_in_for(j).is_some() || slot.comes_with(j).is_some() {
                return Err(format!("apart cannot name `{c}`: it stands in for or comes with another"));
            }
        }
        if slot1.follows(j1) {
            return Err(format!(
                "`{first}` is named first in apart, so it gives way, but it takes no turns: name a base or an extra second"
            ));
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
#[derive(Clone)]
enum State {
    /// One element on: which, the elements' weights, and the turns each is owed.
    One { on: usize, weights: Vec<f64>, owed: Vec<f64> },
    /// The post slot: each element on or off, and which one a swap switches next.
    Each(Vec<bool>, usize),
}

impl State {
    fn swappable(&self) -> bool {
        match self {
            // A stand-in weighs 0 and takes no turns.
            State::One { weights, .. } => weights.iter().filter(|w| **w > 0.0).count() > 1,
            State::Each(on, _) => !on.is_empty(),
        }
    }

    /// A slot with one element on, the first of weight above 0, as though the round robin had
    /// just picked it.
    fn one(weights: Vec<f64>) -> State {
        let total: f64 = weights.iter().sum();
        let on = weights.iter().position(|w| *w > 0.0).unwrap_or(0);
        let mut owed = weights.clone();
        if let Some(first) = owed.get_mut(on) {
            *first -= total;
        }
        State::One { on, weights, owed }
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
                // A stand-in, a companion, an extra or the base weighs 0, so the turns of the others
                // stay what they are without it.
                State::one(
                    (0..s.elements.len()).map(|i| if s.follows(i) { 0.0 } else { s.weight(i) }).collect(),
                )
            }
        })
        .collect();
    let swappable: Vec<usize> =
        state.iter().enumerate().filter(|(_, s)| s.swappable()).map(|(i, _)| i).collect();
    // The slots an empty section may keep: all but the post slot.
    let content: Vec<usize> =
        slots.iter().enumerate().filter(|(_, s)| s.name != POST).map(|(i, _)| i).collect();
    let stages = stages.max(1);
    let cols = columns(slots);
    let mut turn = 0usize;
    let mut walk = 0.0f64;
    let mut dense = 0usize;
    let mut track = 0usize;
    let mut stage = 0usize;
    let mut onward = true;
    let mut switched = 0.0f64;
    // Per stage, the stretches of breaks and builds it has shown, and whether the stretch
    // going on opens the walls.
    let mut stretches = vec![0usize; stages];
    let mut open = false;
    let mut calm_before = false;
    // The same for the extras, per kind: lively, calm, steady.
    let mut runs = [Extra::Lively, Extra::Calm, Extra::Steady].map(|kind| Run {
        kind,
        stretches: vec![0; stages],
        open: false,
        before: false,
    });
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
            let may_switch = part.track_start || part.after_break;
            if stages > 1 && may_switch && change >= STAGE_CHANGE && start - switched >= rules.stage_gap {
                (stage, onward) = next_stage(stage, onward, stages);
                switched = start;
            }
            if big || part.whole {
                for &s in &swappable {
                    if let (Some(x), Some(slot)) = (state.get_mut(s), slots.get(s)) {
                        x.swap_all(&|j| slot.takes_turns(j, stage));
                    }
                }
            } else if change >= CHANGE_SMALL
                && let Some(&s) = swappable.get(turn % swappable.len().max(1))
            {
                if let (Some(x), Some(slot)) = (state.get_mut(s), slots.get(s)) {
                    x.swap(&|j| slot.takes_turns(j, stage));
                }
                turn += 1;
            }
            // An element on for its dwell gives way, its slot's turn or not.
            if !big && !part.whole && change >= CHANGE_SMALL && rules.dwell > 0 {
                for (s, (x, slot)) in state.iter_mut().zip(slots).enumerate() {
                    let unswapped = x.on().is_some() && x.on() == before.get(s).copied().flatten();
                    if unswapped && since.get(s).is_some_and(|n| *n >= rules.dwell) {
                        x.swap(&|j| slot.takes_turns(j, stage));
                    }
                }
            }
        }
        // An element left on where the stage does not show it gives way to the next one there.
        for (x, slot) in state.iter_mut().zip(slots) {
            if let State::One { on, .. } = x
                && !slot.shows_in(*on, stage)
            {
                x.swap(&|j| slot.takes_turns(j, stage));
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
        // Whether the extras of each kind come in this row: every second stretch of their kind.
        let mut now = Vec::with_capacity(runs.len());
        for run in &mut runs {
            let fits = run.kind.fits(part.role, part.energy);
            if fits && !run.before {
                let seen = run.stretches.get(stage).copied().unwrap_or(0);
                run.open = seen % 2 == 0;
                if let Some(n) = run.stretches.get_mut(stage) {
                    *n += 1;
                }
            }
            run.before = fits;
            if fits && run.open {
                now.push(run.kind);
            }
        }
        let comes = |slot: &Slot, j: usize| slot.extra_of(j).is_some_and(|x| now.contains(&x));
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
                State::One { on, weights, .. } => presence.extend((0..weights.len()).map(|j| {
                    let base = slot.is_base(j) && !empty;
                    let shown = j == *on || comes(slot, j) || base;
                    if shown && slot.shows_in(j, stage) { level } else { 0.0 }
                })),
                State::Each(on, _) => presence.extend(
                    on.iter()
                        .enumerate()
                        .map(|(j, o)| if *o && !empty && slot.shows_in(j, stage) { 1.0 } else { 0.0 }),
                ),
            }
        }
        // Of a pair kept apart the first gives way for this row. Its slot shows what its next swap
        // would bring; the swap itself stays to come, so the turns stay as without the pair.
        for (first, second) in &rules.apart {
            let (Some(a), Some(b)) =
                (cols.iter().position(|c| c == first), cols.iter().position(|c| c == second))
            else {
                continue;
            };
            let level = presence.get(a).copied().unwrap_or(0.0);
            if level <= 0.0 || presence.get(b).copied().unwrap_or(0.0) <= 0.0 {
                continue;
            }
            if let Some(x) = presence.get_mut(a) {
                *x = 0.0;
            }
            let Some(s) = slots.iter().position(|slot| slot.elements.iter().any(|e| e == first)) else {
                continue;
            };
            let (Some(slot), Some(x)) = (slots.get(s), state.get(s)) else { continue };
            let offset: usize = slots.iter().take(s).map(|o| o.elements.len()).sum();
            let mut ahead = x.clone();
            for _ in 0..slot.elements.len() {
                ahead.swap(&|j| slot.takes_turns(j, stage));
                let Some(j) = ahead.on() else { break };
                let Some(name) = slot.elements.get(j) else { break };
                if name == first {
                    break;
                }
                let clash = rules.apart.iter().any(|(p, q)| {
                    let partner = if p == name {
                        q
                    } else if q == name {
                        p
                    } else {
                        return false;
                    };
                    cols.iter()
                        .position(|c| c == partner)
                        .and_then(|k| presence.get(k))
                        .is_some_and(|v| *v > 0.0)
                });
                if !clash {
                    if let Some(v) = presence.get_mut(offset + j) {
                        *v = level;
                    }
                    break;
                }
            }
        }
        // A stand-in takes over the presence of the element it stands in for, on its stages.
        let mut at = 0usize;
        for slot in slots {
            for j in 0..slot.elements.len() {
                if let Some(other) = slot.stands_in_for(j)
                    && slot.shows_in(j, stage)
                    && let Some(k) = cols.iter().position(|c| c == other)
                {
                    let taken = presence.get(k).copied().unwrap_or(0.0);
                    if let Some(x) = presence.get_mut(k) {
                        *x = 0.0;
                    }
                    if let Some(x) = presence.get_mut(at + j) {
                        *x = taken;
                    }
                }
            }
            at += slot.elements.len();
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
        let flight = match part.role {
            Role::Break => FLIGHT_STRAIGHT,
            Role::Drop => FLIGHT_ROLL,
            Role::Build => FLIGHT_BANK,
            Role::Intro | Role::Outro | Role::Main => {
                FLIGHT_VARIANTS.get(track % FLIGHT_VARIANTS.len()).copied().unwrap_or(FLIGHT_SLALOM)
            }
        };
        let pattern = match part.role {
            Role::Intro | Role::Outro | Role::Break => PATTERN_STRIPES,
            Role::Build => PATTERN_CHASE,
            Role::Drop => PATTERN_PULSE,
            Role::Main => {
                PATTERN_VARIANTS.get(track % PATTERN_VARIANTS.len()).copied().unwrap_or(PATTERN_STRIPES)
            }
        };
        let calm = matches!(part.role, Role::Break | Role::Build);
        if calm && !calm_before {
            let seen = stretches.get(stage).copied().unwrap_or(0);
            open = seen % 2 == 0;
            if let Some(n) = stretches.get_mut(stage) {
                *n += 1;
            }
        }
        calm_before = calm;
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
            flight,
            pattern,
            see_through: if calm && open { SEE_THROUGH } else { 0.0 },
            empty,
        });
    }
    // The walls stay closed next to a change of stage, and no companion or extra comes there.
    let extras: Vec<usize> = slots
        .iter()
        .flat_map(|s| (0..s.elements.len()).map(move |j| s.extra_of(j).is_some()))
        .enumerate()
        .filter(|(_, x)| *x)
        .map(|(i, _)| i)
        .collect();
    let at: Vec<usize> = out.iter().map(|r| r.stage).collect();
    for (i, row) in out.iter_mut().enumerate() {
        let before = i.checked_sub(1).and_then(|k| at.get(k));
        let after = at.get(i + 1);
        let turning = before.is_some_and(|s| *s != row.stage) || after.is_some_and(|s| *s != row.stage);
        if turning {
            row.see_through = 0.0;
            for &k in &extras {
                if let Some(x) = row.presence.get_mut(k) {
                    *x = 0.0;
                }
            }
        }
        let calm_enough = match parts.get(i).map(|p| (p.role, p.energy)) {
            Some((Role::Build, _)) => true,
            Some((Role::Main, energy)) => energy.is_finite() && energy < COMPANION_ENERGY,
            _ => false,
        };
        if !turning && calm_enough {
            come_along(slots, &cols, row);
        }
    }
    out
}

/// The companions in `row`: each one on its stages shows with the presence of the element it
/// comes with, where that element is the only one of its slot on (companions aside), and the
/// glider then flies through it.
fn come_along(slots: &[Slot], cols: &[String], row: &mut Row) {
    let mut at = 0usize;
    for slot in slots {
        for j in 0..slot.elements.len() {
            let Some(other) = slot.comes_with(j) else { continue };
            if !slot.shows_in(j, row.stage) {
                continue;
            }
            let Some(k) = cols.iter().position(|c| c == other) else { continue };
            let level = row.presence.get(k).copied().unwrap_or(0.0);
            // The other element's slot: whether anything else of it is on.
            let mut first = 0usize;
            let mut alone = false;
            for s in slots {
                let n = s.elements.len();
                if (first..first + n).contains(&k) {
                    alone = (0..n).all(|e| {
                        first + e == k
                            || s.comes_with(e).is_some()
                            || s.extra_of(e).is_some()
                            || s.is_base(e)
                            || row.presence.get(first + e).is_none_or(|p| *p <= 0.0)
                    });
                    break;
                }
                first += n;
            }
            if level > 0.0 && alone {
                if let Some(x) = row.presence.get_mut(at + j) {
                    *x = level;
                }
                row.flight = FLIGHT_THREAD;
            }
        }
        at += slot.elements.len();
    }
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
        cells.push(r.flight.to_string());
        cells.push(r.pattern.to_string());
        cells.push(num(r.see_through));
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
                    after_break: false,
                    whole: false,
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
            "start,end,role,change,blocks,mountains,galaxy,warmth,intensity,camera,stage,gaps,flight,pattern,see_through"
        );
        assert_eq!(lines[1], "0,30,intro,0,0.4,0,1,0,0.3,4,0,0,0,0,0");
        assert_eq!(lines[2], "30,60,drop,0.9,0,1,1,0.45,1,2,0,0,2,1,0");
        assert_eq!(lines.len(), 3);
        assert!(check(&slots()).is_ok());
        let bad = |e: &[&str]| vec![Slot::new("floor", e)];
        assert!(check(&bad(&["a", "a"])).unwrap_err().contains("twice"));
        assert!(check(&bad(&["warmth"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["camera"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["stage"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["gaps"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["see_through"])).unwrap_err().contains("column"));
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
    fn the_flight_follows_the_role_and_each_track_takes_a_variant() {
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
            (Role::Main, 0.5),
        ]);
        p[0].track_start = true;
        p[6].track_start = true;
        p[8].track_start = true;
        p[9].track_start = true;
        let r = rows(&p, &slots(), 1);
        let flights: Vec<usize> = r.iter().map(|r| r.flight).collect();
        let [a, b, c] = FLIGHT_VARIANTS;
        assert_eq!(flights, [a, a, FLIGHT_BANK, FLIGHT_ROLL, FLIGHT_STRAIGHT, a, b, b, c, a]);
        // The column comes last, as a whole number.
        let text = table(&slots(), &r);
        assert!(text.lines().next().is_some_and(|h| h.ends_with(",gaps,flight,pattern,see_through")));
        assert!(
            text.lines().nth(4).is_some_and(|l| l.ends_with(&format!(",{FLIGHT_ROLL},{PATTERN_PULSE},0")))
        );
    }

    #[test]
    fn the_pattern_follows_the_role_and_each_track_takes_a_variant_in_a_main_section() {
        let mut p = parts(&[
            (Role::Intro, 0.0),
            (Role::Main, 0.2),
            (Role::Build, 0.3),
            (Role::Drop, 0.9),
            (Role::Break, 0.6),
            (Role::Outro, 0.3),
            (Role::Intro, 0.9),
            (Role::Main, 0.5),
            (Role::Main, 0.9),
        ]);
        p[0].track_start = true;
        p[6].track_start = true;
        p[8].track_start = true;
        let r = rows(&p, &slots(), 1);
        let patterns: Vec<usize> = r.iter().map(|r| r.pattern).collect();
        let [a, b] = PATTERN_VARIANTS;
        let calm = PATTERN_STRIPES;
        assert_eq!(patterns, [calm, a, PATTERN_CHASE, PATTERN_PULSE, calm, calm, calm, b, a]);
        // The column comes last, as a whole number.
        let text = table(&slots(), &r);
        assert!(text.lines().next().is_some_and(|h| h.ends_with(",flight,pattern,see_through")));
        assert!(text.lines().nth(3).is_some_and(|l| l.ends_with(&format!(",{PATTERN_CHASE},1"))));
    }

    #[test]
    fn every_second_stretch_of_breaks_and_builds_sees_through_the_walls_but_not_next_to_a_stage_change() {
        let roles = [
            (Role::Intro, 0.0),
            (Role::Break, 0.5),
            (Role::Build, 0.3),
            (Role::Drop, 0.9),
            (Role::Break, 0.6),
            (Role::Main, 0.4),
            (Role::Build, 0.5),
            (Role::Drop, 0.9),
            (Role::Outro, 0.3),
        ];
        let r = rows(&parts(&roles), &slots(), 1);
        let open: Vec<f64> = r.iter().map(|r| r.see_through).collect();
        // Stretches: rows 1 and 2 (open), row 4 (closed), row 6 (open).
        let s = SEE_THROUGH;
        assert_eq!(open, [0.0, s, s, 0.0, 0.0, 0.0, s, 0.0, 0.0]);
        let text = table(&slots(), &r);
        assert!(text.lines().nth(2).is_some_and(|l| l.ends_with(",1")), "{text}");
        assert!(text.lines().nth(5).is_some_and(|l| l.ends_with(",0")), "{text}");

        // Two stages: each counts its own stretches, and the rows on either side of a change
        // stay closed.
        let mut p = parts(&[
            (Role::Intro, 0.0),
            (Role::Break, 0.5),
            (Role::Main, 0.4),
            (Role::Break, 0.5),
            (Role::Intro, 0.9),
            (Role::Break, 0.5),
            (Role::Main, 0.4),
            (Role::Break, 0.5),
            (Role::Main, 0.4),
            (Role::Break, 0.5),
            (Role::Intro, 0.9),
        ]);
        for (i, part) in p.iter_mut().enumerate() {
            part.start = i as f64 * STAGE_GAP;
            part.end = part.start + STAGE_GAP;
            part.zone = (part.start, part.start);
        }
        p[4].track_start = true;
        p[10].track_start = true;
        let r = rows(&p, &slots(), 2);
        let stage: Vec<usize> = r.iter().map(|r| r.stage).collect();
        assert_eq!(stage, [0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 0]);
        let open: Vec<f64> = r.iter().map(|r| r.see_through).collect();
        // Stage 0: rows 1 (open) and 3 (its second stretch, closed; the change follows anyway).
        // Stage 1: rows 5 (open), 7 (closed), 9 (open but before the change: closed).
        assert_eq!(open, [0.0, s, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0, 0.0, 0.0]);
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
        assert!(t.lines().nth(5).is_some_and(|l| l.ends_with(",0.6,1,0,0")), "{t}");
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
                after_break: false,
                whole: false,
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

    #[test]
    fn an_extra_comes_in_every_second_stretch_of_its_rows_and_leaves_every_other_cell_as_it_was() {
        // 80 parts, a track every 10, so the stage walks 0, 1, 2, 1, 0, 1, 2, 1; the roles in an
        // uneven cycle so that stretches of calm and of lively rows follow each other.
        let mut p = Vec::new();
        let cycle = [
            Role::Intro,
            Role::Main,
            Role::Main,
            Role::Drop,
            Role::Break,
            Role::Build,
            Role::Drop,
            Role::Outro,
        ];
        for i in 0..80 {
            let start = i as f64 * 60.0;
            let track_start = i % 10 == 0;
            p.push(Part {
                start,
                end: start + 60.0,
                zone: (start, start),
                role: cycle[i % cycle.len()],
                change: if track_start { 0.9 } else { 0.4 },
                energy: if i % 3 == 0 { 0.9 } else { 0.5 },
                track_start,
                seed: i as u32,
                after_break: false,
                whole: false,
            });
        }
        let sky = Slot::new("sky", &["galaxy", "planet"]);
        let plain = vec![Slot::new("mid", &["glider", "lasers"]), sky.clone()];
        let mut mid = Slot::new("mid", &["glider", "lasers", "rings"]);
        mid.stages = vec![vec![], vec![], vec![2]];
        mid.extra = vec![None, None, Some(Extra::Lively)];
        let mut far = Slot::new("sky", &["galaxy", "planet", "ships"]);
        far.stages = vec![vec![], vec![], vec![2]];
        far.extra = vec![None, None, Some(Extra::Calm)];
        let with = vec![mid, far];
        assert_eq!(check(&with), Ok(()));
        let before = rows(&p, &plain, 3);
        let after = rows(&p, &with, 3);
        assert_eq!(before.len(), after.len());
        // Columns before: glider lasers galaxy planet; after: glider lasers rings galaxy planet ships.
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            for (x, y) in [(0, 0), (1, 1), (3, 2), (4, 3)] {
                assert_eq!(a.presence[x], b.presence[y], "every other cell as before: {a:?} {b:?}");
            }
            assert_eq!((a.stage, a.camera, a.warmth, a.flight), (b.stage, b.camera, b.warmth, b.flight));
            let turning = (i > 0 && after[i - 1].stage != a.stage)
                || after.get(i + 1).is_some_and(|n| n.stage != a.stage);
            for (col, kind, slot) in [(2, Extra::Lively, MID), (5, Extra::Calm, SKY)] {
                let x = a.presence[col];
                if a.stage != 2 || turning || !kind.fits(p[i].role, p[i].energy) {
                    assert_eq!(x, 0.0, "only on its stage, in its rows, not at a change: {i} {a:?}");
                } else if x > 0.0 {
                    // The slot's presence for the row: a break keeps the sky and the mid slot at 0.6.
                    let want = if a.role == Role::Break { BREAK_SKY } else { 1.0 };
                    assert_eq!(x, want, "{slot} {i} {a:?}");
                }
            }
        }
        // Every second stretch of each kind on the ascent shows, the first one included.
        for (col, kind) in [(2, Extra::Lively), (5, Extra::Calm)] {
            let mut stretches: Vec<Vec<f64>> = Vec::new();
            let mut last: Option<usize> = None;
            for (i, a) in after.iter().enumerate() {
                if a.stage == 2 && kind.fits(p[i].role, p[i].energy) {
                    if last != Some(i.wrapping_sub(1)) || after[i - 1].stage != 2 {
                        stretches.push(Vec::new());
                    }
                    if let Some(s) = stretches.last_mut() {
                        s.push(a.presence[col]);
                    }
                    last = Some(i);
                }
            }
            assert!(stretches.len() >= 3, "{kind:?}: {stretches:?}");
            for (n, s) in stretches.iter().enumerate() {
                let on = s.iter().any(|x| *x > 0.0);
                if n % 2 == 1 {
                    assert!(!on, "{kind:?}: every second stretch stays out: {stretches:?}");
                }
            }
            assert!(
                stretches.iter().step_by(2).filter(|s| s.iter().any(|x| *x > 0.0)).count() >= 2,
                "{kind:?}: {stretches:?}"
            );
        }
        // What an extra may not be.
        let mut post = Slot::new("post", &["a"]);
        post.extra = vec![Some(Extra::Calm)];
        assert!(check(&[Slot::new("sky", &["c"]), post]).is_err_and(|e| e.contains("post")));
        let mut both = Slot::new("mid", &["a", "b"]);
        both.extra = vec![None, Some(Extra::Lively)];
        both.with = vec![None, Some("a".into())];
        assert!(check(&[both]).is_err_and(|e| e.contains("extra")));
        let mut only = Slot::new("mid", &["a"]);
        only.extra = vec![Some(Extra::Lively)];
        assert!(check(&[only, Slot::new("sky", &["c"])]).is_err_and(|e| e.contains("only stand-ins")));
        let mut follow = Slot::new("mid", &["a", "b", "d"]);
        follow.extra = vec![None, Some(Extra::Lively), None];
        follow.instead = vec![None, None, Some("b".into())];
        assert!(check(&[follow]).is_err_and(|e| e.contains("is an extra")));
        assert_eq!(Extra::named("calm"), Some(Extra::Calm));
        assert_eq!(Extra::named("loud"), None);
    }

    #[test]
    fn a_steady_extra_comes_in_every_second_stretch_of_calmer_mains_and_never_with_another_extra() {
        let main = |energy: f64| (Role::Main, energy);
        let roles = [
            (Role::Intro, 0.5),
            main(0.5),
            main(0.5),
            (Role::Drop, 0.9),
            main(0.5),
            main(0.9),
            main(0.5),
            (Role::Break, 0.3),
            main(0.5),
            (Role::Outro, 0.3),
            main(0.5),
            main(0.5),
        ];
        let mut p = parts(&roles.map(|(role, _)| (role, 0.4)));
        for (part, (_, energy)) in p.iter_mut().zip(roles) {
            part.energy = energy;
        }
        let plain = vec![Slot::new("mid", &["glider", "lasers"]), Slot::new("sky", &["galaxy", "planet"])];
        let mut mid = Slot::new("mid", &["glider", "lasers", "rings"]);
        mid.extra = vec![None, None, Some(Extra::Lively)];
        let mut sky = Slot::new("sky", &["galaxy", "planet", "ships", "structure"]);
        sky.extra = vec![None, None, Some(Extra::Calm), Some(Extra::Steady)];
        let with = vec![mid, sky];
        assert_eq!(check(&with), Ok(()));
        assert_eq!(Extra::named("steady"), Some(Extra::Steady));
        let before = rows(&p, &plain, 1);
        let after = rows(&p, &with, 1);
        // Columns after: glider lasers rings galaxy planet ships structure.
        for (b, a) in before.iter().zip(&after) {
            for (x, y) in [(0, 0), (1, 1), (3, 2), (4, 3)] {
                assert_eq!(a.presence[x], b.presence[y], "every other cell as before: {a:?} {b:?}");
            }
            let extras = [2, 5, 6].iter().filter(|&&k| a.presence[k] > 0.0).count();
            assert!(extras <= 1, "never two extras at once: {a:?}");
        }
        // Steady stretches: rows 1-2, 4, 6, 8, 10-11; the first, third and fifth show.
        let steady: Vec<f64> = after.iter().map(|r| r.presence[6]).collect();
        assert_eq!(steady, [0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0]);
        assert!(!Extra::Steady.fits(Role::Main, 0.8) && !Extra::Steady.fits(Role::Build, 0.5));
        assert!(!Extra::Steady.fits(Role::Main, f64::NAN));
    }

    #[test]
    fn a_stand_in_takes_over_where_its_element_is_on_and_leaves_every_other_cell_as_it_was() {
        // The bars in the sky on every stage, the rings in the mid slot standing in for them on
        // stage 1; the same 40 parts as above, so the stage walks 0, 1, 2, 1.
        let mut p = Vec::new();
        for i in 0..40 {
            let start = i as f64 * 60.0;
            let track_start = i % 10 == 0;
            let role = [Role::Main, Role::Break, Role::Drop, Role::Intro][i % 4];
            p.push(Part {
                start,
                end: start + 60.0,
                zone: (start, start),
                role,
                change: if track_start { 0.9 } else { 0.4 },
                energy: 0.6,
                track_start,
                seed: i as u32,
                after_break: false,
                whole: false,
            });
        }
        let sky = Slot::new("sky", &["galaxy", "planet", "bars"]);
        let plain = vec![Slot::new("mid", &["glider", "lasers"]), sky.clone()];
        let mut mid = Slot::new("mid", &["glider", "lasers", "rings"]);
        mid.stages = vec![vec![], vec![], vec![1]];
        mid.instead = vec![None, None, Some("bars".into())];
        let with = vec![mid, sky];
        assert_eq!(check(&with), Ok(()));
        let before = rows(&p, &plain, 3);
        let after = rows(&p, &with, 3);
        assert_eq!(before.len(), after.len());
        let mut took = 0;
        for (b, a) in before.iter().zip(&after) {
            // Columns before: glider lasers galaxy planet bars; after: glider lasers rings galaxy
            // planet bars.
            let (bars, rings) = (a.presence[5], a.presence[2]);
            if a.stage == 1 {
                assert_eq!(bars, 0.0, "the bars never show beside the rings: {a:?}");
                assert_eq!(rings, b.presence[4], "the rings show as the bars would: {a:?}");
                took += usize::from(rings > 0.0);
            } else {
                assert_eq!(rings, 0.0, "the rings only on their stage: {a:?}");
                assert_eq!(bars, b.presence[4], "{a:?}");
            }
            for (x, y) in [(0, 0), (1, 1), (3, 2), (4, 3)] {
                assert_eq!(a.presence[x], b.presence[y], "every other cell as before: {a:?} {b:?}");
            }
            assert_eq!((a.stage, a.camera, a.warmth), (b.stage, b.camera, b.warmth));
        }
        assert!(took > 0, "the rings take over somewhere");
        // What a stand-in may not be.
        let bad = |slot: &str, of: &str| {
            let mut s = Slot::new(slot, &["a", "b"]);
            s.instead = vec![None, Some(of.into())];
            check(&[s, Slot::new("sky", &["c"])])
        };
        assert!(bad("mid", "x").is_err_and(|e| e.contains("no element")));
        assert!(bad("mid", "b").is_err_and(|e| e.contains("itself")));
        assert!(bad("post", "c").is_err_and(|e| e.contains("post")));
        let mut only = Slot::new("mid", &["a"]);
        only.instead = vec![Some("c".into())];
        assert!(check(&[only, Slot::new("sky", &["c"])]).is_err_and(|e| e.contains("only stand-ins")));
        let mut chain = Slot::new("mid", &["a", "b", "d"]);
        chain.instead = vec![None, Some("c".into()), Some("b".into())];
        assert!(
            check(&[chain, Slot::new("sky", &["c"])])
                .is_err_and(|e| e.contains("stands in for or comes with another"))
        );
    }

    #[test]
    fn a_companion_comes_with_its_element_alone_in_calm_rows_of_its_stage_and_the_glider_threads_it() {
        // Two stages; a track start every 10 parts of 60 s switches the stage.
        let roles = [Role::Main, Role::Build, Role::Drop, Role::Main, Role::Intro];
        let p: Vec<Part> = (0..40)
            .map(|i| {
                let start = i as f64 * 60.0;
                let track_start = i % 10 == 0;
                Part {
                    start,
                    end: start + 60.0,
                    zone: (start, start),
                    role: roles[i % roles.len()],
                    change: if track_start { 0.9 } else { 0.4 },
                    energy: if i % 3 == 0 { 0.9 } else { 0.6 },
                    track_start,
                    seed: i as u32,
                    after_break: false,
                    whole: false,
                }
            })
            .collect();
        let sky = Slot::new("sky", &["galaxy", "planet"]);
        let mut mid = Slot::new("mid", &["glider", "lasers", "traps"]);
        mid.weights = vec![3.0, 1.0, 1.0];
        let plain =
            vec![Slot { elements: vec!["glider".into(), "lasers".into()], ..mid.clone() }, sky.clone()];
        mid.stages = vec![vec![], vec![], vec![1]];
        mid.with = vec![None, None, Some("glider".into())];
        let with = vec![mid, sky];
        assert_eq!(check(&with), Ok(()));
        let before = rows(&p, &plain, 2);
        let after = rows(&p, &with, 2);
        assert_eq!(before.len(), after.len());
        let (mut came, mut left_out) = (0, 0);
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            // Columns before: glider lasers galaxy planet; after: glider lasers traps galaxy planet.
            for (x, y) in [(0, 0), (1, 1), (3, 2), (4, 3)] {
                assert_eq!(a.presence[x], b.presence[y], "every other cell as before: {a:?} {b:?}");
            }
            assert_eq!(
                (a.stage, a.camera, a.warmth, a.see_through),
                (b.stage, b.camera, b.warmth, b.see_through)
            );
            let steady = (i == 0 || after[i - 1].stage == a.stage)
                && after.get(i + 1).is_none_or(|n| n.stage == a.stage);
            let calm = a.role == Role::Build || (a.role == Role::Main && p[i].energy < COMPANION_ENERGY);
            let alone = a.presence[0] > 0.0 && a.presence[1] == 0.0;
            if a.stage == 1 && steady && calm && alone {
                assert_eq!(a.presence[2], a.presence[0], "the traps come with the glider: {a:?}");
                assert_eq!(a.flight, FLIGHT_THREAD, "{a:?}");
                came += 1;
            } else {
                assert_eq!(a.presence[2], 0.0, "{i}: {a:?}");
                assert_eq!(a.flight, b.flight, "{i}: {a:?}");
                left_out += usize::from(a.stage == 1 && alone);
            }
        }
        assert!(came > 0 && left_out > 0, "{came} {left_out}");
        // What a companion may not be.
        let bad = |slot: &str, of: &str| {
            let mut s = Slot::new(slot, &["a", "b"]);
            s.with = vec![None, Some(of.into())];
            check(&[s, Slot::new("sky", &["c"])])
        };
        assert!(bad("mid", "x").is_err_and(|e| e.contains("no element")));
        assert!(bad("mid", "b").is_err_and(|e| e.contains("itself")));
        assert!(bad("post", "c").is_err_and(|e| e.contains("post")));
        let mut both = Slot::new("mid", &["a", "b"]);
        both.instead = vec![None, Some("c".into())];
        both.with = vec![None, Some("a".into())];
        assert!(check(&[both, Slot::new("sky", &["c"])]).is_err_and(|e| e.contains("not both")));
    }

    #[test]
    fn a_pair_kept_apart_never_shares_a_row_and_leaves_every_other_cell_as_it_was() {
        let slots = [
            Slot::new("floor", &["stones", "steps", "gates"]),
            Slot::new("mid", &["rings", "smoke"]),
            Slot::new("sky", &["stars"]),
        ];
        let of: Vec<(Role, f64)> = (0..50)
            .map(|i| (if i % 7 == 6 { Role::Intro } else { Role::Main }, 0.3 + 0.1 * (i % 5) as f64))
            .collect();
        let p = parts(&of);
        let without = rows(&p, &slots, 1);
        let cols = columns(&slots);
        let at = |c: &str| cols.iter().position(|x| x == c).unwrap();
        let (gates, rings) = (at("gates"), at("rings"));
        let met = without.iter().filter(|r| r.presence[gates] > 0.0 && r.presence[rings] > 0.0).count();
        assert!(met > 0, "the test needs rows where both are on");
        let rules = Rules { apart: vec![("gates".into(), "rings".into())], ..Rules::default() };
        check_apart(&slots, &rules.apart).unwrap();
        let with = rows_with(&p, &slots, 1, rules);
        assert_eq!(with.len(), without.len());
        for (a, b) in with.iter().zip(&without) {
            assert!(a.presence[gates] == 0.0 || a.presence[rings] == 0.0, "{a:?}");
            let met_here = b.presence[gates] > 0.0 && b.presence[rings] > 0.0;
            if met_here {
                // The gates' presence went to another element of the floor; the rings stay.
                assert_eq!(a.presence[rings], b.presence[rings]);
                let others = a.presence[at("stones")].max(a.presence[at("steps")]);
                assert_eq!(others, b.presence[gates]);
                assert_eq!(a.presence.iter().take(3).filter(|v| **v > 0.0).count(), 1);
            } else {
                assert_eq!(a, b);
            }
            assert_eq!(a.warmth, b.warmth);
            assert_eq!(a.camera, b.camera);
        }
        // The first named only: naming them the other way round moves the rings.
        let turned = Rules { apart: vec![("rings".into(), "gates".into())], ..Rules::default() };
        let other = rows_with(&p, &slots, 1, turned);
        assert!(other.iter().zip(&without).any(|(a, b)| a.presence[gates] == b.presence[gates]
            && b.presence[gates] > 0.0
            && a.presence[rings] == 0.0
            && b.presence[rings] > 0.0));
        // With nothing else to show the first simply goes.
        let alone = [Slot::new("floor", &["gates"]), Slot::new("mid", &["rings"])];
        let rules = Rules { apart: vec![("gates".into(), "rings".into())], ..Rules::default() };
        for r in rows_with(&p, &alone, 1, rules) {
            assert!(r.presence[0] == 0.0 || r.presence[1] == 0.0, "{r:?}");
        }
        let two = |a: &str, b: &str| vec![(a.to_string(), b.to_string())];
        assert!(check_apart(&slots, &two("gates", "nope")).unwrap_err().contains("no element"));
        assert!(check_apart(&slots, &two("gates", "stones")).unwrap_err().contains("one slot"));
        let mut based = slots.clone();
        based[0].base = vec![false, false, true];
        assert!(check_apart(&based, &two("gates", "rings")).unwrap_err().contains("takes no turns"));
        assert!(check_apart(&based, &two("rings", "gates")).is_ok());
    }

    #[test]
    fn a_base_stays_on_under_every_turn_of_its_slot_and_leaves_every_other_cell_as_it_was() {
        // Two stages; a track start every 10 parts of 60 s switches the stage.
        let roles = [Role::Intro, Role::Main, Role::Build, Role::Drop, Role::Break, Role::Main, Role::Outro];
        let p: Vec<Part> = (0..50)
            .map(|i| {
                let start = i as f64 * 60.0;
                let track_start = i % 10 == 0;
                Part {
                    start,
                    end: start + 60.0,
                    zone: (start, start),
                    role: roles[i % roles.len()],
                    change: if track_start {
                        0.9
                    } else if i % 2 == 0 {
                        0.5
                    } else {
                        0.2
                    },
                    energy: if i % 3 == 0 { 0.9 } else { 0.6 },
                    track_start,
                    seed: i as u32,
                    after_break: false,
                    whole: false,
                }
            })
            .collect();
        let mut mid = Slot::new("mid", &["glider", "traps"]);
        mid.with = vec![None, Some("glider".into())];
        let plain = vec![
            Slot::new("floor", &["terraces", "stelae"]),
            mid.clone(),
            Slot::new("sky", &["glow", "clouds"]),
        ];
        // The reeds as the floor's base, on every stage; the stars as the sky's, on stage 1 only,
        // first in the list; a mid base beside the glider, so its companion must not count it.
        let mut floor = Slot::new("floor", &["terraces", "reeds", "stelae"]);
        floor.base = vec![false, true, false];
        let mut sky = Slot::new("sky", &["stars", "glow", "clouds"]);
        sky.base = vec![true];
        sky.stages = vec![vec![1]];
        let mut mid_base = Slot::new("mid", &["glider", "traps", "smoke"]);
        mid_base.with = vec![None, Some("glider".into()), None];
        mid_base.base = vec![false, false, true];
        let with = vec![floor, mid_base, sky];
        assert_eq!(check(&with), Ok(()));
        let before = rows(&p, &plain, 2);
        let after = rows(&p, &with, 2);
        assert_eq!(before.len(), after.len());
        // Columns before: terraces stelae glider traps glow clouds;
        // after: terraces reeds stelae glider traps smoke stars glow clouds.
        let (mut shown, mut dark) = (0, 0);
        for (b, a) in before.iter().zip(&after) {
            for (x, y) in [(0, 0), (2, 1), (3, 2), (4, 3), (7, 4), (8, 5)] {
                assert_eq!(a.presence[x], b.presence[y], "every other cell as before: {a:?} {b:?}");
            }
            assert_eq!(
                (a.stage, a.camera, a.warmth, a.flight, a.empty),
                (b.stage, b.camera, b.warmth, b.flight, b.empty)
            );
            // A base takes its slot's presence for the row; an empty row keeps no base.
            let floor = b.presence[0] + b.presence[1];
            let mid = b.presence[2];
            let sky = b.presence[4] + b.presence[5];
            assert_eq!(a.presence[1], if a.empty { 0.0 } else { floor }, "the reeds: {a:?} {b:?}");
            assert_eq!(a.presence[5], if a.empty { 0.0 } else { mid }, "the smoke: {a:?} {b:?}");
            let stars = if a.empty || a.stage != 1 { 0.0 } else { sky };
            assert_eq!(a.presence[6], stars, "the stars, on stage 1 only: {a:?} {b:?}");
            if a.presence[1] > 0.0 {
                shown += 1;
            } else {
                dark += 1;
            }
        }
        assert!(shown > 20 && dark > 3, "{shown} {dark}");
        assert!(after.iter().any(|r| r.role == Role::Intro && r.presence[1] == CALM_PRESENCE));
        assert!(after.iter().any(|r| r.role == Role::Break && r.stage == 1 && r.presence[6] == BREAK_SKY));
        assert!(after.iter().any(|r| r.presence[4] > 0.0), "the traps still come with the glider");
        // An empty row keeps one element and no base.
        let s = vec![
            Slot { base: vec![false, false, true], ..Slot::new("floor", &["a", "b", "r"]) },
            Slot::new("sky", &["s"]),
        ];
        let seed = (0..10_000u32).find(|&k| draw(k, 3) < 0.3 * EMPTY_CHANCE).unwrap_or(0);
        let mut q = parts(&[(Role::Drop, 0.0), (Role::Main, 0.1), (Role::Main, 0.1), (Role::Main, 1.0)]);
        q[3].seed = seed;
        let r = rows(&q, &s, 1);
        assert!(r[3].empty);
        assert_eq!(r[3].presence.iter().filter(|p| **p > 0.0).count(), 1, "{:?}", r[3].presence);
        assert_eq!(r[3].presence[2], 0.0);
        assert_eq!(r[2].presence, [1.0, 0.0, 1.0, 1.0], "the base on beside the first element");
        // What a base may not be.
        let mut two = Slot::new("floor", &["a", "b", "c"]);
        two.base = vec![true, false, true];
        assert!(
            check(&[two]).is_err_and(|e| e.contains("`a`") && e.contains("`c`") && e.contains("two bases"))
        );
        let mut post = Slot::new("post", &["a", "b"]);
        post.base = vec![true];
        assert!(check(&[Slot::new("sky", &["c"]), post]).is_err_and(|e| e.contains("post")));
        let mut lone = Slot::new("floor", &["a"]);
        lone.base = vec![true];
        assert!(check(&[lone]).is_err_and(|e| e.contains("needs an element of its own")));
        let mut extra = Slot::new("floor", &["a", "b"]);
        extra.base = vec![false, true];
        extra.extra = vec![None, Some(Extra::Calm)];
        assert!(check(&[extra]).is_err_and(|e| e.contains("base")));
        let mut on_base = Slot::new("floor", &["a", "b", "c"]);
        on_base.base = vec![false, true, false];
        on_base.instead = vec![None, None, Some("b".into())];
        assert!(check(&[on_base]).is_err_and(|e| e.contains("or a base")));
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
                after_break: false,
                whole: false,
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

    /// Bar starts every 2 s up to `n` bars, and the file's end.
    fn edges(n: usize) -> Vec<f64> {
        (0..=n).map(|i| i as f64 * 2.0).collect()
    }

    #[test]
    fn a_stretch_without_bass_counts_only_with_bass_on_both_sides() {
        // Silence for 6 bars, bass, 12 bars without, bass, a fade of 3 bars.
        let mut bass = vec![-95.0; 6];
        bass.extend(vec![-24.0; 60]);
        bass.extend(vec![-40.0; 12]);
        bass.extend(vec![-24.0; 60]);
        bass.extend(vec![-60.0; 3]);
        let e = edges(bass.len());
        assert_eq!(bass_gone(&e, &bass), vec![(132.0, 156.0)]);
        // Two bars are a fill; one bar with bass inside does not end the stretch.
        let mut dips = vec![-24.0; 40];
        dips[10] = -40.0;
        dips[11] = -40.0;
        for bar in [20, 21, 23, 24] {
            dips[bar] = -40.0;
        }
        assert_eq!(bass_gone(&edges(40), &dips), vec![(40.0, 50.0)]);
        // No bass to lose, and no numbers at all.
        assert!(bass_gone(&edges(40), &[-90.0; 40]).is_empty());
        assert!(bass_gone(&[], &[]).is_empty());
    }

    fn at(start: f64, end: f64, role: Role, change: f64, track_start: bool) -> Part {
        Part {
            start,
            end,
            zone: (start, start),
            role,
            change,
            energy: 0.8,
            track_start,
            seed: 1,
            after_break: false,
            whole: false,
        }
    }

    /// The bars of every 2 s up to `end`.
    fn bars_to(end: usize) -> Vec<f64> {
        (0..end / 2).map(|i| i as f64 * 2.0).collect()
    }

    #[test]
    fn a_found_break_cuts_the_row_it_lies_in_and_takes_back_a_false_track_start() {
        let p = [
            at(0.0, 80.0, Role::Drop, 0.0, true),
            at(80.0, 142.0, Role::Outro, 0.3, false),
            at(142.0, 212.0, Role::Intro, 1.0, true),
            at(212.0, 300.0, Role::Main, 0.6, false),
        ];
        // One bar after the section start: the break takes the start.
        let out = with_breaks(&p, &[(144.0, 168.0)], &bars_to(300), &|_, _| 0.5);
        let starts: Vec<(f64, f64, Role)> = out.iter().map(|x| (x.start, x.end, x.role)).collect();
        assert_eq!(
            starts,
            vec![
                (0.0, 80.0, Role::Drop),
                (80.0, 142.0, Role::Main),
                (142.0, 168.0, Role::Break),
                (168.0, 212.0, Role::Drop),
                (212.0, 300.0, Role::Main),
            ]
        );
        assert!(!out[2].track_start && !out[3].track_start);
        assert!(out[3].after_break && !out[2].after_break);
        assert_eq!(out[2].change, 1.0, "the boundary keeps its stronger change");
        assert_eq!(out[3].change, DROP_CHANGE);
        assert_eq!(out[3].zone, (166.0, 168.0), "the fade has arrived on the drop");
        assert_eq!(out[3].energy, 0.5);
        // In the middle of a section, away from any boundary: it is cut in two around the break.
        let out = with_breaks(&p, &[(100.0, 120.0)], &bars_to(300), &|_, _| 0.5);
        assert_eq!(out[1].end, 100.0);
        assert_eq!(out[1].role, Role::Outro);
        assert_eq!((out[2].start, out[2].end, out[2].change), (100.0, 120.0, BREAK_CHANGE));
        assert_eq!((out[3].start, out[3].end), (120.0, 142.0));
        assert!(out[4].track_start, "a track start after the drop stays");
        // A section start just after the drop goes: no row of a bar stays.
        let out = with_breaks(&p, &[(120.0, 140.0)], &bars_to(300), &|_, _| 0.5);
        assert_eq!((out[2].start, out[2].end), (120.0, 140.0));
        assert_eq!((out[3].start, out[3].end, out[3].role), (140.0, 212.0, Role::Drop));
    }

    #[test]
    fn split_cuts_at_bars_and_keeps_breaks_whole() {
        // 22 bars, 3 bars, 12 bars: 8 bars each, the rest of 6 stays apart, 3 bars stay whole.
        let p = [
            at(0.0, 44.0, Role::Drop, 0.0, true),
            at(44.0, 50.0, Role::Main, 0.5, false),
            at(50.0, 74.0, Role::Break, 0.5, false),
            at(74.0, 94.0, Role::Outro, 0.5, false),
        ];
        let out = split(&p, &bars_to(100), 8, &|from, _| from / 100.0);
        let got: Vec<(f64, f64, Role, f64)> =
            out.iter().map(|x| (x.start, x.end, x.role, x.change)).collect();
        assert_eq!(
            got,
            vec![
                (0.0, 16.0, Role::Drop, 0.0),
                (16.0, 32.0, Role::Main, SPLIT_CHANGE),
                (32.0, 44.0, Role::Main, SPLIT_CHANGE),
                (44.0, 50.0, Role::Main, 0.5),
                (50.0, 74.0, Role::Break, 0.5),
                (74.0, 94.0, Role::Outro, 0.5),
            ]
        );
        assert!(out[0].track_start && !out[1].track_start);
        let whole: Vec<bool> = out.iter().map(|x| x.whole).collect();
        assert_eq!(whole, vec![false, false, false, true, true, true]);
        assert_eq!(out[1].zone, (14.0, 16.0));
        assert_eq!(out[1].energy, 0.16);
        // A rest under half of `every` stays with the piece before it.
        let long = [at(0.0, 20.0, Role::Main, 0.0, false)];
        let out = split(&long, &bars_to(100), 8, &|_, _| 0.5);
        assert_eq!(out.len(), 1);
        let long = [at(0.0, 24.0, Role::Main, 0.0, false)];
        let out = split(&long, &bars_to(100), 8, &|_, _| 0.5);
        assert_eq!((out.len(), out[0].end), (2, 16.0));
        // A long blend into the next section leaves the first half of the row before it alone.
        let mut blend = [at(0.0, 32.0, Role::Main, 0.0, false), at(32.0, 48.0, Role::Main, 0.3, false)];
        blend[1].zone = (16.0, 32.0);
        let out = split(&blend, &bars_to(100), 8, &|_, _| 0.5);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].zone, (24.0, 32.0));
        // 0 leaves the parts alone.
        assert_eq!(split(&p, &bars_to(100), 0, &|_, _| 0.5), p);
    }

    #[test]
    fn a_split_boundary_swaps_one_slot_and_moves_no_stage() {
        let slots =
            vec![Slot::new("floor", &["blocks", "mountains"]), Slot::new("sky", &["galaxy", "aurora"])];
        let p = [at(0.0, 48.0, Role::Main, 0.0, false)];
        let cut = split(&p, &bars_to(100), 8, &|_, _| 0.8);
        let r = rows_with(&cut, &slots, 2, Rules { dwell: 0, stage_gap: 0.0, ..Rules::default() });
        assert_eq!(r.len(), 3);
        let on = |row: &Row| row.presence.iter().map(|x| *x > 0.0).collect::<Vec<bool>>();
        // The slots take turns.
        assert_eq!(on(&r[0]), vec![true, false, true, false]);
        assert_eq!(on(&r[1]), vec![false, true, true, false]);
        assert_eq!(on(&r[2]), vec![false, true, false, true]);
        assert!(r.iter().all(|x| x.stage == 0));
        assert!(r[1].walk > r[0].walk && r[1].walk < 0.1);
    }

    #[test]
    fn a_weak_boundary_between_sections_swaps_more_than_a_cut_inside_one() {
        let slots =
            vec![Slot::new("floor", &["blocks", "mountains"]), Slot::new("sky", &["galaxy", "aurora"])];
        let p = [at(0.0, 32.0, Role::Main, 0.0, false), at(32.0, 64.0, Role::Main, 0.1, false)];
        let cut = split(&p, &bars_to(100), 8, &|_, _| 0.8);
        let r = rows_with(&cut, &slots, 1, Rules { dwell: 0, ..Rules::default() });
        assert_eq!(r.len(), 4);
        let on = |row: &Row| row.presence.iter().map(|x| *x > 0.0).collect::<Vec<bool>>();
        // The cut inside the first section swaps the floor, the boundary after it both slots.
        assert_eq!(on(&r[1]), vec![false, true, true, false]);
        assert_eq!(on(&r[2]), vec![true, false, false, true]);
    }

    #[test]
    fn a_whole_boundary_never_empties_a_row() {
        let slots =
            vec![Slot::new("floor", &["blocks", "mountains"]), Slot::new("sky", &["galaxy", "aurora"])];
        for seed in 0..64 {
            let mut p = [
                at(0.0, 32.0, Role::Main, 0.0, false),
                at(32.0, 64.0, Role::Main, 0.3, false),
                at(64.0, 96.0, Role::Main, 0.3, false),
                at(96.0, 128.0, Role::Main, 0.3, false),
                at(128.0, 160.0, Role::Main, 0.1, false),
            ];
            p[4].seed = seed;
            p[4].whole = true;
            let r = rows_with(&p, &slots, 1, Rules::default());
            assert!(!r[4].empty, "seed {seed}");
        }
    }

    #[test]
    fn the_stage_changes_on_the_drop_after_a_break_within_the_gap_given() {
        let p = [
            at(0.0, 142.0, Role::Main, 0.0, false),
            at(142.0, 168.0, Role::Break, 0.5, false),
            Part { after_break: true, ..at(168.0, 300.0, Role::Drop, DROP_CHANGE, false) },
        ];
        let slots = slots();
        let stages = |rules: Rules| -> Vec<usize> {
            rows_with(&p, &slots, 2, rules).iter().map(|r| r.stage).collect()
        };
        // The old lock of 480 s holds it; a shorter gap lets it go on the drop, and only there.
        assert_eq!(stages(Rules::default()), vec![0, 0, 0]);
        assert_eq!(stages(Rules { stage_gap: 120.0, ..Rules::default() }), vec![0, 0, 1]);
        // A shorter gap alone: a drop that follows no found break keeps the stage.
        let mut plain = p;
        plain[2].after_break = false;
        let r = rows_with(&plain, &slots, 2, Rules { stage_gap: 120.0, ..Rules::default() });
        assert!(r.iter().all(|x| x.stage == 0));
    }
}
