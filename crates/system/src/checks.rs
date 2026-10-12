//! Settings of the show machine that can ruin a show, found by `system.check` and fixed one at
//! a time by `system.fix` ("deactivate gaming overlay" as part of the software). Every fix is
//! reversible: it reports what it replaced ([`Previous`]), and `system.restore` writes that back.
//!
//! Checks so far (Windows, current user only, no administrator rights needed):
//! - `gaming_overlay`: Game Bar and game capture. With them on, Windows took a fullscreen output
//!   for a game and popped up "Get an app to open this 'ms-gamingoverlay' link" during testing.
//! - `notifications`: toast notifications, which pop up over whatever is on the main display.

use penumbra_core::Error;
use serde::{Deserialize, Serialize};

/// One registry value a check looks at (a DWORD under `HKEY_CURRENT_USER`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    pub key: &'static str,
    pub name: &'static str,
    /// The value that is safe for a show.
    pub wanted: u32,
}

/// What a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    /// Worth fixing before a show.
    Problem,
    /// Could not be read.
    Unknown,
}

/// The result of one check.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub status: CheckStatus,
    /// What was found, one line.
    pub detail: String,
    /// Why it matters and what the fix does.
    pub explanation: &'static str,
    /// The values read, as `key\name = value` (`not set` for missing values).
    pub values: Vec<String>,
}

/// What a fix replaced, so `system.restore` can put it back. `None`: the value did not exist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Previous {
    pub id: String,
    pub values: Vec<PreviousValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousValue {
    pub key: String,
    pub name: String,
    pub value: Option<u32>,
}

/// Where the settings live; the real registry on Windows, a map in tests.
pub trait Registry {
    fn get(&self, key: &str, name: &str) -> Result<Option<u32>, Error>;
    fn set(&mut self, key: &str, name: &str, value: u32) -> Result<(), Error>;
    fn delete(&mut self, key: &str, name: &str) -> Result<(), Error>;
}

struct Definition {
    id: &'static str,
    title: &'static str,
    explanation: &'static str,
    settings: &'static [Setting],
}

const DEFINITIONS: &[Definition] = &[
    Definition {
        id: "gaming_overlay",
        title: "Gaming overlay",
        explanation: "Windows can take a fullscreen output for a game and pop up the Game Bar or \
                      an 'ms-gamingoverlay' message over it. The fix turns off game capture and \
                      the Game Bar for the current user.",
        settings: &[
            Setting { key: r"System\GameConfigStore", name: "GameDVR_Enabled", wanted: 0 },
            Setting {
                key: r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
                name: "AppCaptureEnabled",
                wanted: 0,
            },
            Setting { key: r"Software\Microsoft\GameBar", name: "UseNexusForGameBarEnabled", wanted: 0 },
        ],
    },
    Definition {
        id: "notifications",
        title: "Notifications",
        explanation: "Toast notifications (updates, mail, chat) pop up on the main display. The fix \
                      turns notification banners off for the current user (Settings, System, \
                      Notifications).",
        settings: &[Setting {
            key: r"Software\Microsoft\Windows\CurrentVersion\PushNotifications",
            name: "ToastEnabled",
            wanted: 0,
        }],
    },
];

fn definition(id: &str) -> Result<&'static Definition, Error> {
    DEFINITIONS.iter().find(|d| d.id == id).ok_or_else(|| {
        let ids: Vec<&str> = DEFINITIONS.iter().map(|d| d.id).collect();
        Error::new(format!("no check `{id}`; checks: {}", ids.join(", ")))
    })
}

fn show(v: Option<u32>) -> String {
    v.map_or_else(|| "not set".to_string(), |v| v.to_string())
}

/// Runs every check against `registry`.
pub fn run_with(registry: &dyn Registry) -> Vec<Check> {
    DEFINITIONS.iter().map(|d| check_one(d, registry)).collect()
}

fn check_one(d: &Definition, registry: &dyn Registry) -> Check {
    let mut values = Vec::new();
    let mut wrong = Vec::new();
    let mut error = None;
    for s in d.settings {
        match registry.get(s.key, s.name) {
            Ok(v) => {
                values.push(format!(r"{}\{} = {}", s.key, s.name, show(v)));
                // A missing value means Windows' default, which is "on" for all of these.
                if v != Some(s.wanted) {
                    wrong.push(s.name);
                }
            }
            Err(e) => {
                values.push(format!(r"{}\{} = ?", s.key, s.name));
                error.get_or_insert(e.0);
            }
        }
    }
    let (status, detail) = match (error, wrong.is_empty()) {
        (Some(e), _) => (CheckStatus::Unknown, e),
        (None, true) => (CheckStatus::Ok, "off".to_string()),
        (None, false) => (CheckStatus::Problem, format!("on ({})", wrong.join(", "))),
    };
    Check { id: d.id, title: d.title, status, detail, explanation: d.explanation, values }
}

/// Applies the fix for check `id`; returns what it replaced.
pub fn fix_with(id: &str, registry: &mut dyn Registry) -> Result<Previous, Error> {
    let d = definition(id)?;
    let mut previous = Previous { id: d.id.to_string(), values: Vec::new() };
    // Read everything first, so a failing read changes nothing.
    for s in d.settings {
        let value = registry.get(s.key, s.name)?;
        previous.values.push(PreviousValue { key: s.key.into(), name: s.name.into(), value });
    }
    for s in d.settings {
        registry.set(s.key, s.name, s.wanted)?;
    }
    log::info!("system.fix {id}: replaced {previous:?}");
    Ok(previous)
}

/// Writes back what a fix replaced. Only settings the check owns are accepted, so this cannot
/// be used to write arbitrary registry values.
pub fn restore_with(previous: &Previous, registry: &mut dyn Registry) -> Result<(), Error> {
    let d = definition(&previous.id)?;
    for v in &previous.values {
        if !d.settings.iter().any(|s| s.key == v.key && s.name == v.name) {
            return Err(Error::new(format!(r"`{}\{}` is not part of check `{}`", v.key, v.name, d.id)));
        }
    }
    for v in &previous.values {
        match v.value {
            Some(value) => registry.set(&v.key, &v.name, value)?,
            None => registry.delete(&v.key, &v.name)?,
        }
    }
    log::info!("system.restore {}: wrote back {:?}", d.id, previous.values);
    Ok(())
}

/// The machine's registry (Windows); elsewhere every value reads as unknown and writes fail.
pub struct SystemRegistry;

impl Registry for SystemRegistry {
    fn get(&self, key: &str, name: &str) -> Result<Option<u32>, Error> {
        #[cfg(windows)]
        {
            crate::windows::get_dword(key, name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key, name);
            Err(Error::new("no system checks on this platform yet"))
        }
    }

    fn set(&mut self, key: &str, name: &str, value: u32) -> Result<(), Error> {
        #[cfg(windows)]
        {
            crate::windows::set_dword(key, name, value)
        }
        #[cfg(not(windows))]
        {
            let _ = (key, name, value);
            Err(Error::new("no system fixes on this platform yet"))
        }
    }

    fn delete(&mut self, key: &str, name: &str) -> Result<(), Error> {
        #[cfg(windows)]
        {
            crate::windows::delete_value(key, name)
        }
        #[cfg(not(windows))]
        {
            let _ = (key, name);
            Err(Error::new("no system fixes on this platform yet"))
        }
    }
}

/// The checks of this machine (none on platforms without a backend yet).
pub fn run() -> Vec<Check> {
    if cfg!(windows) { run_with(&SystemRegistry) } else { Vec::new() }
}

pub fn fix(id: &str) -> Result<Previous, Error> {
    fix_with(id, &mut SystemRegistry)
}

pub fn restore(previous: &Previous) -> Result<(), Error> {
    restore_with(previous, &mut SystemRegistry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Fake(BTreeMap<(String, String), u32>);

    impl Registry for Fake {
        fn get(&self, key: &str, name: &str) -> Result<Option<u32>, Error> {
            Ok(self.0.get(&(key.into(), name.into())).copied())
        }
        fn set(&mut self, key: &str, name: &str, value: u32) -> Result<(), Error> {
            self.0.insert((key.into(), name.into()), value);
            Ok(())
        }
        fn delete(&mut self, key: &str, name: &str) -> Result<(), Error> {
            self.0.remove(&(key.into(), name.into()));
            Ok(())
        }
    }

    #[test]
    fn fix_and_restore() {
        let mut reg = Fake::default();
        reg.set(r"System\GameConfigStore", "GameDVR_Enabled", 1).unwrap();
        let before = run_with(&reg);
        assert_eq!(before[0].status, CheckStatus::Problem);
        assert!(before[0].detail.contains("GameDVR_Enabled"), "{}", before[0].detail);
        assert_eq!(before[1].status, CheckStatus::Problem);

        let previous = fix_with("gaming_overlay", &mut reg).unwrap();
        assert_eq!(run_with(&reg)[0].status, CheckStatus::Ok);
        assert_eq!(previous.values[0].value, Some(1));
        assert_eq!(previous.values[1].value, None);

        // Through JSON, as the UI and Claude hand it back.
        let json = serde_json::to_string(&previous).unwrap();
        restore_with(&serde_json::from_str(&json).unwrap(), &mut reg).unwrap();
        assert_eq!(reg.get(r"System\GameConfigStore", "GameDVR_Enabled").unwrap(), Some(1));
        assert_eq!(reg.get(r"Software\Microsoft\GameBar", "UseNexusForGameBarEnabled").unwrap(), None);
        assert_eq!(run_with(&reg)[0].status, CheckStatus::Problem);
    }

    #[test]
    fn restore_writes_only_its_own_settings() {
        let mut reg = Fake::default();
        let sneaky = Previous {
            id: "notifications".into(),
            values: vec![PreviousValue { key: r"Software\Evil".into(), name: "Run".into(), value: Some(1) }],
        };
        assert!(restore_with(&sneaky, &mut reg).is_err());
        assert!(reg.0.is_empty());
        assert!(fix_with("nope", &mut reg).is_err());
    }
}
