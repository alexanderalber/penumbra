//! The file dialog behind a script node's "choose Python ..." button (`script.choose`): the way
//! out when the search for a Python finds none.

use std::path::PathBuf;

/// What to pick, as the button's hover text.
pub const HINT: &str = if cfg!(windows) {
    "Pick python.exe in the folder of a Python 3.12 or newer. penumbra keeps the choice for every project"
} else if cfg!(target_os = "macos") {
    "Pick a Python 3.12 or newer: its libpython3.XY.dylib, or its python3 (not the one in /usr/bin, which has no library with it). penumbra keeps the choice for every project"
} else {
    "Pick a Python 3.12 or newer: its libpython3.XY.so, or its python3 when the library is in ../lib. penumbra keeps the choice for every project"
};

const TITLE: &str = if cfg!(windows) {
    "python.exe of a Python 3.12 or newer"
} else {
    "libpython3.XY or python3 of a Python 3.12 or newer"
};

/// Where the dialog starts: the folder of the choice so far, else where the usual installer
/// puts its Pythons, if that exists.
fn start_dir() -> Option<PathBuf> {
    let chosen = penumbra_engine::script_choice::chosen()
        .and_then(|p| if p.is_dir() { Some(p) } else { p.parent().map(PathBuf::from) });
    let usual = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Python"))
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Frameworks/Python.framework/Versions"))
    } else {
        None
    };
    chosen.into_iter().chain(usual).find(|d| d.is_dir())
}

/// Asks for the file and chooses its Python. Blocks until the dialog closes, so it runs on a
/// thread of its own; what it returns goes to the status bar.
pub fn ask() -> String {
    let mut dialog = rfd::FileDialog::new().set_title(TITLE);
    if let Some(dir) = start_dir() {
        dialog = dialog.set_directory(dir);
    }
    let Some(file) = dialog.pick_file() else {
        return "no Python chosen".to_string();
    };
    match penumbra_engine::script_choice::choose(Some(&file)) {
        Ok(outcome) => outcome.message,
        Err(e) => e.0,
    }
}
