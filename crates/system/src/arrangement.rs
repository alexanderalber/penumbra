//! The show-machine check `display_arrangement`: whether the connected displays extend the
//! desktop. With Duplicate (clone) the projectors mirror the main display; with "PC screen
//! only" or "Second screen only" some connected displays stay dark.
//!
//! Windows stores one arrangement per combination of connected displays. Unplugging one
//! projector switches to the arrangement stored for the displays that remain, which may be
//! Duplicate and then takes every projector's picture away. Only the current combination can be
//! seen and set from here; the check's text says so.

use penumbra_core::Error;

use crate::checks::{Check, CheckStatus, Previous, PreviousValue};

pub const ID: &str = "display_arrangement";
const TITLE: &str = "Display arrangement";
const EXPLANATION: &str = "With Duplicate, the projectors show the main display's picture. With \
    'PC screen only' or 'Second screen only', some connected displays stay dark. Windows stores \
    one arrangement for each combination of connected displays. This check sees only the \
    combination connected now. Before a show, unplug each projector once and choose Extend with \
    Win+P for the displays that remain. Then a loose cable darkens only its own projector. The \
    fix sets Extend for the displays connected now.";

/// The `key` and `name` a fix's [`Previous`] uses for the arrangement it replaced.
const KEY: &str = "display";
const NAME: &str = "arrangement";

/// The arrangements of Win+P (Windows' `DISPLAYCONFIG_TOPOLOGY_ID`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topology {
    /// "PC screen only": the built-in display.
    Internal,
    /// "Duplicate".
    Clone,
    /// "Extend".
    Extend,
    /// "Second screen only": the external displays.
    External,
}

impl Topology {
    pub fn from_raw(raw: i32) -> Option<Topology> {
        match raw {
            1 => Some(Topology::Internal),
            2 => Some(Topology::Clone),
            4 => Some(Topology::Extend),
            8 => Some(Topology::External),
            _ => None,
        }
    }

    pub fn raw(self) -> i32 {
        match self {
            Topology::Internal => 1,
            Topology::Clone => 2,
            Topology::Extend => 4,
            Topology::External => 8,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Topology::Internal => "PC screen only",
            Topology::Clone => "duplicate",
            Topology::Extend => "extend",
            Topology::External => "second screen only",
        }
    }
}

/// A display connector as Windows names it: adapter and target or source id.
pub type Id = (i64, u32);

/// How the connected displays are arranged now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    /// What Win+P shows for the displays connected now. `None` when Windows reports none of its
    /// four (a custom arrangement from the display settings) or it could not be read.
    pub stored: Option<Topology>,
    /// Every active path: the desktop source and the connector it shows on. Two paths with the
    /// same source show the same picture.
    pub active: Vec<(Id, Id)>,
    /// The connectors with a display attached, on or off.
    pub connected: Vec<Id>,
}

/// Where the arrangement comes from: Windows' display configuration, a fake in tests.
pub trait Displays {
    fn arrangement(&self) -> Result<Arrangement, Error>;
    /// Switches to `topology` as Win+P does.
    fn set(&mut self, topology: Topology) -> Result<(), Error>;
}

/// The machine's display configuration (Windows); elsewhere reading and setting fail.
pub struct SystemDisplays;

impl Displays for SystemDisplays {
    fn arrangement(&self) -> Result<Arrangement, Error> {
        #[cfg(windows)]
        {
            crate::windows::arrangement()
        }
        #[cfg(not(windows))]
        {
            Err(Error::new("no display arrangement on this platform yet"))
        }
    }

    fn set(&mut self, topology: Topology) -> Result<(), Error> {
        #[cfg(windows)]
        {
            crate::windows::set_topology(topology)
        }
        #[cfg(not(windows))]
        {
            let _ = topology;
            Err(Error::new("no display arrangement on this platform yet"))
        }
    }
}

fn counts(a: &Arrangement) -> (usize, usize, usize) {
    let mut sources: Vec<Id> = a.active.iter().map(|(s, _)| *s).collect();
    let mut on: Vec<Id> = a.active.iter().map(|(_, t)| *t).collect();
    let mut connected = a.connected.clone();
    for v in [&mut sources, &mut on, &mut connected] {
        v.sort_unstable();
        v.dedup();
    }
    // A display that is on is connected, even if the list of connectors missed it.
    let connected = connected.len().max(on.len());
    (sources.len(), on.len(), connected)
}

/// Judges an arrangement read from `displays`.
pub fn check_with(displays: &dyn Displays) -> Check {
    let (status, detail, values) = match displays.arrangement() {
        Ok(a) => {
            let (pictures, on, connected) = counts(&a);
            let stored = a.stored.map_or("custom or unknown", Topology::word);
            let values = vec![
                format!("Win+P = {stored}"),
                format!("displays on = {on} of {connected} connected"),
                format!("different pictures = {pictures}"),
            ];
            let (status, detail) = if on > pictures {
                (CheckStatus::Problem, format!("duplicate: {on} displays show {pictures} picture(s)"))
            } else if connected > on {
                (
                    CheckStatus::Problem,
                    format!("{} of {connected} connected displays are off", connected - on),
                )
            } else if connected <= 1 {
                (CheckStatus::Ok, "one display".to_string())
            } else {
                (CheckStatus::Ok, format!("extend, {on} displays"))
            };
            (status, detail, values)
        }
        Err(e) => (CheckStatus::Unknown, e.0, Vec::new()),
    };
    Check { id: ID, title: TITLE, status, detail, explanation: EXPLANATION, values }
}

/// Sets Extend; the [`Previous`] names the arrangement Win+P showed before (`None` when it was a
/// custom one, which a restore cannot bring back).
pub fn fix_with(displays: &mut dyn Displays) -> Result<Previous, Error> {
    let before = displays.arrangement()?;
    displays.set(Topology::Extend)?;
    let value = before.stored.map(|t| t.raw() as u32);
    log::info!("system.fix {ID}: replaced {:?}", before.stored);
    Ok(Previous {
        id: ID.to_string(),
        values: vec![PreviousValue { key: KEY.into(), name: NAME.into(), value }],
    })
}

/// Switches back to the arrangement a fix replaced.
pub fn restore_with(previous: &Previous, displays: &mut dyn Displays) -> Result<(), Error> {
    let [v] = previous.values.as_slice() else {
        return Err(Error::new(format!("check `{ID}` restores exactly one value")));
    };
    if v.key != KEY || v.name != NAME {
        return Err(Error::new(format!(r"`{}\{}` is not part of check `{ID}`", v.key, v.name)));
    }
    let Some(raw) = v.value else {
        return Err(Error::new(
            "the arrangement before the fix was a custom one; set it again in Windows' display settings",
        ));
    };
    let topology = i32::try_from(raw)
        .ok()
        .and_then(Topology::from_raw)
        .ok_or_else(|| Error::new(format!("{raw} is not a Win+P arrangement")))?;
    displays.set(topology)?;
    log::info!("system.restore {ID}: back to {topology:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        now: Arrangement,
        set: Vec<Topology>,
    }

    impl Displays for Fake {
        fn arrangement(&self) -> Result<Arrangement, Error> {
            Ok(self.now.clone())
        }
        fn set(&mut self, topology: Topology) -> Result<(), Error> {
            self.set.push(topology);
            Ok(())
        }
    }

    fn fake(stored: Option<Topology>, active: &[(u32, u32)], connected: &[u32]) -> Fake {
        Fake {
            now: Arrangement {
                stored,
                active: active.iter().map(|&(s, t)| ((7, s), (7, t))).collect(),
                connected: connected.iter().map(|&t| (7, t)).collect(),
            },
            set: Vec::new(),
        }
    }

    #[test]
    fn duplicate_and_dark_displays_are_problems() {
        // The laptop: panel and two projectors, the projectors mirror the panel.
        let c = check_with(&fake(Some(Topology::Clone), &[(0, 10), (0, 11), (0, 12)], &[10, 11, 12]));
        assert_eq!(c.status, CheckStatus::Problem);
        assert!(c.detail.contains("duplicate: 3 displays show 1"), "{}", c.detail);

        // "PC screen only" with two projectors connected.
        let c = check_with(&fake(Some(Topology::Internal), &[(0, 10)], &[10, 11, 12]));
        assert_eq!(c.status, CheckStatus::Problem);
        assert!(c.detail.contains("2 of 3 connected displays are off"), "{}", c.detail);

        // A custom arrangement that mirrors two of three is still a duplicate.
        let c = check_with(&fake(None, &[(0, 10), (1, 11), (1, 12)], &[10, 11, 12]));
        assert_eq!(c.status, CheckStatus::Problem);
        assert!(c.values[0].contains("custom"), "{:?}", c.values);
    }

    #[test]
    fn extend_and_a_single_display_are_ok() {
        let c = check_with(&fake(Some(Topology::Extend), &[(0, 10), (1, 11), (2, 12)], &[10, 11, 12]));
        assert_eq!((c.status, c.detail.as_str()), (CheckStatus::Ok, "extend, 3 displays"));
        let c = check_with(&fake(Some(Topology::Internal), &[(0, 10)], &[10]));
        assert_eq!((c.status, c.detail.as_str()), (CheckStatus::Ok, "one display"));
        // A connector list that missed an active display does not count it as off.
        let c = check_with(&fake(Some(Topology::Extend), &[(0, 10), (1, 11)], &[10]));
        assert_eq!(c.status, CheckStatus::Ok);
    }

    #[test]
    fn fix_sets_extend_and_restore_goes_back() {
        let mut d = fake(Some(Topology::Clone), &[(0, 10), (0, 11)], &[10, 11]);
        let previous = fix_with(&mut d).unwrap();
        assert_eq!(d.set, [Topology::Extend]);
        let json = serde_json::to_string(&previous).unwrap();
        restore_with(&serde_json::from_str(&json).unwrap(), &mut d).unwrap();
        assert_eq!(d.set, [Topology::Extend, Topology::Clone]);
    }

    #[test]
    fn restore_refuses_what_it_cannot_bring_back() {
        let mut d = fake(None, &[(0, 10), (0, 11)], &[10, 11]);
        let previous = fix_with(&mut d).unwrap();
        assert!(restore_with(&previous, &mut d).is_err());
        let one = |key: &str, value| Previous {
            id: ID.into(),
            values: vec![PreviousValue { key: key.into(), name: NAME.into(), value }],
        };
        assert!(restore_with(&one("Software\\Evil", Some(2)), &mut d).is_err());
        assert!(restore_with(&one(KEY, Some(3)), &mut d).is_err());
        assert_eq!(d.set, [Topology::Extend]);
    }

    /// Reads this machine's arrangement and validates Extend without applying it
    /// (`SetDisplayConfig` with `SDC_VALIDATE` only). Run by hand:
    /// `cargo test -p penumbra-system -- --ignored --nocapture arrangement_of_this_machine`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn arrangement_of_this_machine() {
        let c = check_with(&SystemDisplays);
        println!("{c:#?}");
        println!("validate extend: {:?}", crate::windows::validate_topology(Topology::Extend));
    }
}
