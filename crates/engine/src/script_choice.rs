//! The Python chosen in the app for script nodes (`script.choose`), for a machine where the
//! search finds none or the wrong one.
//!
//! The choice is kept in [`FILE`] in penumbra's data directory, so it holds for every project
//! and survives a restart. It comes after the environment variable `PENUMBRA_PYTHON` and before
//! the search. A process holds one Python interpreter for good: a choice made before any
//! script has run is in effect at once, a later one from the next start.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use penumbra_core::{Error, lock};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;

/// The file's name in [`crate::projects::data_dir`].
pub const FILE: &str = "python.json";

/// A larger file is not read (it holds one path).
const MAX_BYTES: u64 = 64 * 1024;

/// Where the choice is kept; `None` (tests, tools without a data directory): for this run only.
static KEPT_IN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What the file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    /// The chosen Python: a Python directory or a file in one. Empty: none chosen.
    python: String,
}

/// `python.json` in penumbra's data directory (`%LOCALAPPDATA%\penumbra` on Windows).
pub fn default_file() -> Result<PathBuf, Error> {
    Ok(crate::projects::data_dir()?.join(FILE))
}

/// The choice in `file`; none when the file is missing, too large or does not parse.
pub fn read(file: &Path) -> Option<PathBuf> {
    let len = std::fs::metadata(file).ok()?.len();
    if len > MAX_BYTES {
        log::warn!("{} is too large ({len} bytes); no Python is chosen", file.display());
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str::<Kept>(&text) {
        Ok(kept) => Some(kept.python).filter(|p| !p.trim().is_empty()).map(PathBuf::from),
        Err(e) => {
            log::warn!("{} does not parse ({e}); no Python is chosen", file.display());
            None
        }
    }
}

/// Keeps `choice` in `file`.
pub fn write(file: &Path, choice: Option<&Path>) -> Result<(), Error> {
    let kept = Kept { python: choice.map(|p| p.display().to_string()).unwrap_or_default() };
    let text = serde_json::to_string_pretty(&kept).map_err(|e| Error::new(e.to_string()))?;
    crate::show::write_atomic(file, &text)
}

/// Takes the choice kept in `file` and keeps later ones there. Call before the engine starts,
/// which is when Python is looked for.
pub fn load(file: PathBuf) {
    let choice = read(&file);
    if let Some(path) = &choice {
        log::info!("script node: Python chosen in penumbra: {}", path.display());
    }
    penumbra_script::set_chosen(choice);
    *lock(&KEPT_IN) = Some(file);
}

/// The Python chosen so far, if any.
pub fn chosen() -> Option<PathBuf> {
    penumbra_script::chosen()
}

/// What [`choose`] did, for the reply and for the status bar.
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    /// The choice now, `None` after going back to the search.
    pub chosen: Option<String>,
    /// The choice is in effect in this run; otherwise from the next start.
    pub now: bool,
    /// `PENUMBRA_PYTHON` is set and wins over the choice.
    pub overridden: bool,
    /// The file that keeps the choice, `None` when it holds for this run only.
    pub kept_in: Option<String>,
    /// The same in a sentence.
    pub message: String,
    /// The script host as `script.info` shows it.
    pub python: penumbra_script::Status,
}

/// Chooses the Python for script nodes and keeps the choice: a Python directory or a file in
/// one, `None` to go back to the search. A path without a usable Python is refused and changes
/// nothing.
pub fn choose(path: Option<&Path>) -> Result<Outcome, CommandError> {
    let done = penumbra_script::choose(path.map(Path::to_path_buf)).map_err(CommandError)?;
    let kept_in = lock(&KEPT_IN).clone();
    let mut message = match path {
        Some(p) => format!("Python for script nodes: {}", p.display()),
        None => "no Python is chosen any more: script nodes search for one".to_string(),
    };
    if done.overridden {
        message.push_str("; PENUMBRA_PYTHON is set and wins over it");
    } else if !done.now {
        message.push_str("; in effect after a restart of penumbra, since Python has started already");
    }
    let kept_in = match kept_in {
        Some(file) => match write(&file, path) {
            Ok(()) => Some(file.display().to_string()),
            Err(e) => {
                message.push_str(&format!("; not kept for the next start ({e})"));
                None
            }
        },
        None => None,
    };
    Ok(Outcome {
        chosen: path.map(|p| p.display().to_string()),
        now: done.now,
        overridden: done.overridden,
        kept_in,
        message,
        python: penumbra_script::host().status(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChooseArgs {
    path: Option<String>,
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "script.choose",
        "Choose the Python for script nodes when the search finds none or the wrong one, kept for every project and the next starts; \
         {path?: a Python folder or a file in one (python.exe, libpython3.XY.dylib, bin/python3)}, without path: back to the search. \
         In effect at once while no script has run, else after a restart; PENUMBRA_PYTHON wins over it",
        script_choose,
    );
}

fn script_choose(_: &Engine, p: &Json) -> Result<Json, CommandError> {
    let a: ChooseArgs = args(p)?;
    let path = a.path.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(Path::new);
    crate::commands::to_json(&choose(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;

    #[test]
    fn the_choice_is_kept_and_a_broken_file_means_none() {
        let dir = ScratchDir::unborn("script-choice");
        let file = dir.join(FILE);
        assert_eq!(read(&file), None, "no file");
        let python = dir.join("Python313");
        write(&file, Some(&python)).unwrap();
        assert_eq!(read(&file), Some(python));
        write(&file, None).unwrap();
        assert_eq!(read(&file), None, "back to the search");
        std::fs::write(&file, "{ python: ").unwrap();
        assert_eq!(read(&file), None, "does not parse");
    }

    #[test]
    fn a_folder_without_python_is_refused_and_changes_nothing() {
        let dir = ScratchDir::new("script-choice-empty").unwrap();
        let before = penumbra_script::chosen();
        let e = choose(Some(dir.path())).unwrap_err();
        assert!(e.0.contains("the Python chosen in penumbra") && e.0.contains("choose another one"), "{e:?}");
        assert_eq!(penumbra_script::chosen(), before);
    }
}
