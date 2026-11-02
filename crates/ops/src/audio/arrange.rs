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
//! - The colour walks on at every boundary by `change` times [`WALK_STEP`] (twice that from
//!   [`CHANGE_BIG`]) and never walks back: a place grows, and `warmth` goes 0, 1, 0, ... along it.
//! - The role of a section sets the rest: calm in an intro or outro, an empty floor in a break,
//!   everything on in a drop. The sky and the mid slot stay on through a break, dimmed.
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
/// The longest `fade_min` taken, in seconds.
pub const FADE_MIN_MAX: f64 = 30.0;
/// The slot the role rules treat as the sky; every other slot but [`MID`] follows the floor's rules.
pub const SKY: &str = "sky";
/// The slot in front of the camera (a glider, particles). It follows the sky's rules: it stays
/// through a break at [`BREAK_SKY`] and is fully there in an intro or outro, since an empty
/// space before the camera reads as a gap more than an empty floor does.
pub const MID: &str = "mid";
/// The columns before the elements and after them.
pub const LEADING: [&str; 4] = ["start", "end", "role", "change"];
pub const TRAILING: [&str; 2] = ["warmth", "intensity"];
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
}

/// A spatial layer of the stage and its interchangeable elements, in the order they come on.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub name: String,
    /// The elements' column names.
    pub elements: Vec<String>,
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

/// The rows of the arrangement, one per part.
pub fn rows(parts: &[Part], slots: &[Slot]) -> Vec<Row> {
    // Per slot: which element is on and how many it has; which slot swaps next at a medium change.
    let mut on: Vec<(usize, usize)> = slots.iter().map(|s| (0, s.elements.len())).collect();
    let swappable: Vec<usize> = on.iter().enumerate().filter(|(_, (_, n))| *n > 1).map(|(s, _)| s).collect();
    let swap = |slot: Option<&mut (usize, usize)>| {
        if let Some((k, n)) = slot {
            *k = (*k + 1) % (*n).max(1);
        }
    };
    let mut turn = 0usize;
    let mut walk = 0.0f64;
    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let change = if i == 0 { 0.0 } else { unit(part.change) };
        if i > 0 {
            let big = change >= CHANGE_BIG;
            walk += change * WALK_STEP * if big { 2.0 } else { 1.0 };
            if big {
                for &s in &swappable {
                    swap(on.get_mut(s));
                }
            } else if change >= CHANGE_SMALL
                && let Some(&s) = swappable.get(turn % swappable.len().max(1))
            {
                swap(on.get_mut(s));
                turn += 1;
            }
        }
        let mut presence = Vec::new();
        for (slot, &(k, n)) in slots.iter().zip(&on) {
            let sky = slot.name == SKY || slot.name == MID;
            let level = match (part.role, sky) {
                (Role::Intro | Role::Outro, false) => CALM_PRESENCE,
                (Role::Break, false) => 0.0,
                (Role::Break, true) => BREAK_SKY,
                _ => 1.0,
            };
            presence.extend((0..n).map(|j| if j == k { level } else { 0.0 }));
        }
        let intensity = match part.role {
            Role::Intro | Role::Outro => CALM_INTENSITY,
            Role::Drop => 1.0,
            _ => unit(part.energy),
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
    if parts.is_empty() {
        return Err("the file has no sections".into());
    }
    let fade_min = if fade_min.is_finite() { fade_min.clamp(0.0, FADE_MIN_MAX) } else { FADE_MIN };
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
        vec![
            Slot { name: "floor".into(), elements: vec!["blocks".into(), "mountains".into()] },
            Slot { name: "sky".into(), elements: vec!["galaxy".into()] },
        ]
    }

    /// Parts of 30 s each with the given roles and changes, energy 0.6, cuts.
    fn parts(of: &[(Role, f64)]) -> Vec<Part> {
        of.iter()
            .enumerate()
            .map(|(i, &(role, change))| {
                let start = i as f64 * 30.0;
                Part { start, end: start + 30.0, zone: (start, start), role, change, energy: 0.6 }
            })
            .collect()
    }

    /// The presence of `blocks`, `mountains`, `galaxy` per row.
    fn presence(r: &[Row]) -> Vec<[f64; 3]> {
        r.iter().map(|r| [r.presence[0], r.presence[1], r.presence[2]]).collect()
    }

    #[test]
    fn the_first_section_shows_the_first_element_of_each_slot() {
        let r = rows(&parts(&[(Role::Main, 0.9)]), &slots());
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0]]);
        assert_eq!(r[0].warmth, 0.0);
        // Its change does not count: nothing came before.
        assert_eq!(r[0].change, 0.0);
        assert!((r[0].intensity - 0.6).abs() < 1e-12);
    }

    #[test]
    fn a_small_change_walks_the_colour_and_swaps_nothing() {
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.2), (Role::Main, 0.29)]), &slots());
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0]; 3]);
        assert!((r[1].warmth - 0.2 * WALK_STEP).abs() < 1e-12);
        assert!((r[2].warmth - 0.49 * WALK_STEP).abs() < 1e-12);
    }

    #[test]
    fn a_medium_change_swaps_one_slot_and_a_big_one_every_slot_with_a_double_step() {
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.5)]), &slots());
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]);
        assert!((r[1].walk - 0.5 * WALK_STEP).abs() < 1e-12);
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.8)]), &slots());
        assert_eq!(presence(&r), [[1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]);
        assert!((r[1].walk - 0.8 * WALK_STEP * 2.0).abs() < 1e-12);
        // Two floor slots: a big change swaps both, a medium one only one.
        let two = vec![
            Slot { name: "floor".into(), elements: vec!["a".into(), "b".into()] },
            Slot { name: "mid".into(), elements: vec!["c".into(), "d".into(), "e".into()] },
        ];
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 1.0), (Role::Main, 0.5)]), &two);
        assert_eq!(r[1].presence, [0.0, 1.0, 0.0, 1.0, 0.0]);
        assert_eq!(r[2].presence, [1.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn slots_take_turns_and_a_slot_of_one_element_never_swaps() {
        let s = vec![
            Slot { name: "floor".into(), elements: vec!["a".into(), "b".into()] },
            Slot { name: "mid".into(), elements: vec!["c".into(), "d".into(), "e".into()] },
            Slot { name: "sky".into(), elements: vec!["g".into()] },
        ];
        let p = parts(&[
            (Role::Main, 0.0),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
            (Role::Main, 0.5),
        ]);
        let r = rows(&p, &s);
        let on = |r: &Row| -> Vec<usize> {
            r.presence.iter().enumerate().filter(|(_, p)| **p > 0.0).map(|(i, _)| i).collect()
        };
        assert_eq!(on(&r[0]), [0, 2, 5]);
        assert_eq!(on(&r[1]), [1, 2, 5], "floor first");
        assert_eq!(on(&r[2]), [1, 3, 5], "then mid");
        assert_eq!(on(&r[3]), [0, 3, 5], "floor again");
        assert_eq!(on(&r[4]), [0, 4, 5]);
        // Only one-element slots: nothing can swap.
        let single = vec![Slot { name: "sky".into(), elements: vec!["g".into()] }];
        let r = rows(&parts(&[(Role::Main, 0.0), (Role::Main, 0.5), (Role::Main, 1.0)]), &single);
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
        let r = rows(&p, &slots());
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
            Slot { name: "floor".into(), elements: vec!["a".into()] },
            Slot { name: "mid".into(), elements: vec!["g".into()] },
            Slot { name: "post".into(), elements: vec!["p".into()] },
            Slot { name: "sky".into(), elements: vec!["s".into()] },
        ];
        let p = parts(&[(Role::Intro, 0.0), (Role::Break, 0.5), (Role::Drop, 0.9), (Role::Outro, 0.1)]);
        let r: Vec<Vec<f64>> = rows(&p, &s).into_iter().map(|r| r.presence).collect();
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
        let r = rows(&parts(&of), &slots());
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
        );
        assert_eq!(r[1].walk, 0.0);
        assert_eq!(r[2].walk, 0.0);
        assert!((r[3].walk - 2.0 * WALK_STEP).abs() < 1e-12);
    }

    #[test]
    fn the_table_has_a_row_per_section_and_the_columns_named() {
        let p = parts(&[(Role::Intro, 0.0), (Role::Drop, 0.9)]);
        let t = table(&slots(), &rows(&p, &slots()));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "start,end,role,change,blocks,mountains,galaxy,warmth,intensity");
        assert_eq!(lines[1], "0,30,intro,0,0.4,0,1,0,0.3");
        assert_eq!(lines[2], "30,60,drop,0.9,0,1,1,0.45,1");
        assert_eq!(lines.len(), 3);
        assert!(check(&slots()).is_ok());
        let bad = |e: &[&str]| {
            vec![Slot { name: "floor".into(), elements: e.iter().map(|s| s.to_string()).collect() }]
        };
        assert!(check(&bad(&["a", "a"])).unwrap_err().contains("twice"));
        assert!(check(&bad(&["warmth"])).unwrap_err().contains("column"));
        assert!(check(&bad(&["a b"])).unwrap_err().contains("column name"));
        assert!(check(&bad(&[])).unwrap_err().contains("no elements"));
        assert!(check(&[]).is_err());
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
}
