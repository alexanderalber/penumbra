//! Finding a Python for the script node without loading anything: file checks only, so it is
//! cheap enough for startup. The version is confirmed later from the library itself
//! ([`crate::ffi`]).
//!
//! Windows: `PENUMBRA_PYTHON` (a Python directory, or a file in one), else the first
//! directory on `PATH` holding a usable Python, else the newest one in the installer's default
//! places (`%LOCALAPPDATA%\Programs\Python\Python3*`, `%ProgramFiles%\Python3*`). Usable means
//! `python3.dll` next to a `python3XY.dll` with XY >= 12, and the standard library beside it
//! (`Lib\encodings`, or the embeddable distribution's `python3XY.zip`): without it,
//! `Py_Initialize` aborts the process instead of returning an error.
//!
//! Elsewhere (not tested on a real machine yet): `PENUMBRA_PYTHON` (a `libpython3.XY.so*` or
//! a directory with one), else the newest `libpython3.XY.so.1.0` in the usual library
//! directories.

use std::path::{Path, PathBuf};

/// The oldest Python the script node runs on (the stable ABI pyhost is built against).
pub const MIN_MINOR: u32 = 12;
/// Minor versions looked for, newest first.
const MAX_MINOR: u32 = 40;

/// A Python found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    /// The library to load by full path: `python3.dll` on Windows, `libpython3.XY.so*` elsewhere.
    pub library: PathBuf,
    /// The minor version from the file names (confirmed when loaded).
    pub minor: u32,
    /// How it was found: `PENUMBRA_PYTHON`, `PATH`, `default location`.
    pub source: &'static str,
}

/// Where to look; the environment, passed in so tests can vary it.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub explicit: Option<PathBuf>,
    pub path: Vec<PathBuf>,
    /// Directories whose `Python3*` subdirectories are candidates (Windows) or library
    /// directories (elsewhere).
    pub defaults: Vec<PathBuf>,
    /// `PYTHONHOME`, which overrides where Python looks for its standard library.
    pub python_home: Option<PathBuf>,
}

impl Env {
    /// The process environment.
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let defaults = if cfg!(windows) {
            let mut d = Vec::new();
            if let Some(local) = var("LOCALAPPDATA") {
                d.push(local.join("Programs").join("Python"));
            }
            if let Some(pf) = var("ProgramFiles") {
                d.push(pf);
            }
            d
        } else {
            [
                "/usr/lib",
                "/usr/lib64",
                "/usr/local/lib",
                "/usr/lib/x86_64-linux-gnu",
                "/usr/lib/aarch64-linux-gnu",
            ]
            .iter()
            .map(PathBuf::from)
            .collect()
        };
        Env { explicit: var("PENUMBRA_PYTHON"), path, defaults, python_home: var("PYTHONHOME") }
    }
}

/// Finds a usable Python, or says what was looked at and why it was not enough.
pub fn find(env: &Env) -> Result<Install, String> {
    let found = find_install(env)?;
    // PYTHONHOME pointing at another Python makes Py_Initialize abort the process.
    if let Some(home) = &env.python_home {
        let dir = found.library.parent().unwrap_or(Path::new(""));
        if !same_dir(home, dir) {
            return Err(format!(
                "PYTHONHOME is set to {}, but the Python found is in {}; unset PYTHONHOME or point \
                 PENUMBRA_PYTHON at the Python it belongs to",
                home.display(),
                dir.display()
            ));
        }
    }
    Ok(found)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    norm(a) == norm(b)
}

fn find_install(env: &Env) -> Result<Install, String> {
    let mut ignored = Vec::new();
    if let Some(explicit) = &env.explicit {
        let dir = if explicit.is_file() {
            explicit.parent().unwrap_or(explicit).to_path_buf()
        } else {
            explicit.clone()
        };
        let file = explicit.is_file().then_some(explicit.as_path());
        return check(&dir, file, "PENUMBRA_PYTHON", &mut ignored)
            .ok_or_else(|| format!("PENUMBRA_PYTHON={}: {}", explicit.display(), ignored.join("; ")));
    }
    if cfg!(windows) {
        for dir in &env.path {
            if let Some(found) = check(dir, None, "PATH", &mut ignored) {
                return Ok(found);
            }
        }
    }
    // The default places: the newest version wins.
    let mut candidates = Vec::new();
    for base in &env.defaults {
        if cfg!(windows) {
            let Ok(entries) = std::fs::read_dir(base) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("python3") {
                    candidates.push(entry.path());
                }
            }
        } else {
            candidates.push(base.clone());
        }
    }
    let mut best: Option<Install> = None;
    for dir in candidates {
        if let Some(found) = check(&dir, None, "default location", &mut ignored)
            && best.as_ref().is_none_or(|b| found.minor > b.minor)
        {
            best = Some(found);
        }
    }
    best.ok_or_else(|| {
        let mut msg = format!(
            "no Python 3.{MIN_MINOR} or newer found; install one from python.org (or put it on PATH, or set \
             PENUMBRA_PYTHON to its directory)"
        );
        if !ignored.is_empty() {
            msg.push_str("; not usable: ");
            msg.push_str(&ignored.join("; "));
        }
        msg
    })
}

/// Whether `dir` holds a usable Python (`file`: the library the user named, if any).
fn check(
    dir: &Path,
    file: Option<&Path>,
    source: &'static str,
    ignored: &mut Vec<String>,
) -> Option<Install> {
    if cfg!(windows) { check_windows(dir, source, ignored) } else { check_unix(dir, file, source, ignored) }
}

fn check_windows(dir: &Path, source: &'static str, ignored: &mut Vec<String>) -> Option<Install> {
    let stable = dir.join("python3.dll");
    if !stable.is_file() {
        if source == "PENUMBRA_PYTHON" {
            ignored.push(format!("no python3.dll in {}", dir.display()));
        }
        return None;
    }
    let Some(minor) = (MIN_MINOR..=MAX_MINOR).rev().find(|m| dir.join(format!("python3{m}.dll")).is_file())
    else {
        let old = (0..MIN_MINOR).rev().find(|m| dir.join(format!("python3{m}.dll")).is_file());
        ignored.push(match old {
            Some(m) => format!("{} is Python 3.{m}", dir.display()),
            None => format!("{} has python3.dll but no python3XY.dll", dir.display()),
        });
        return None;
    };
    let stdlib = dir.join("Lib").join("encodings").join("__init__.py").is_file()
        || dir.join(format!("python3{minor}.zip")).is_file();
    if !stdlib {
        ignored.push(format!("{} (Python 3.{minor}) has no standard library beside it", dir.display()));
        return None;
    }
    Some(Install { library: stable, minor, source })
}

fn check_unix(
    dir: &Path,
    file: Option<&Path>,
    source: &'static str,
    ignored: &mut Vec<String>,
) -> Option<Install> {
    if let Some(file) = file {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return match unix_minor(&name) {
            Some(minor) if minor >= MIN_MINOR => Some(Install { library: file.to_path_buf(), minor, source }),
            Some(minor) => {
                ignored.push(format!("{} is Python 3.{minor}", file.display()));
                None
            }
            None => {
                ignored.push(format!("{} is not a libpython3.XY.so", file.display()));
                None
            }
        };
    }
    for minor in (MIN_MINOR..=MAX_MINOR).rev() {
        for name in [format!("libpython3.{minor}.so.1.0"), format!("libpython3.{minor}.so")] {
            let p = dir.join(&name);
            if p.is_file() {
                return Some(Install { library: p, minor, source });
            }
        }
    }
    if source == "PENUMBRA_PYTHON" {
        ignored.push(format!("no libpython3.XY.so (XY >= {MIN_MINOR}) in {}", dir.display()));
    }
    None
}

/// `libpython3.12.so.1.0` -> 12.
fn unix_minor(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("libpython3.")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !rest.get(digits.len()..)?.starts_with(".so") {
        return None;
    }
    digits.parse().ok()
}

/// `3.12.10 (tags/...` -> (3, 12).
pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> penumbra_core::scratch::ScratchDir {
        penumbra_core::scratch::ScratchDir::new(&format!("locate-{name}")).unwrap()
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("3.12.10 (tags/v3.12.10:0cc8128, Apr  8 2025)"), Some((3, 12)));
        assert_eq!(parse_version("3.13.0rc1"), Some((3, 13)));
        assert_eq!(parse_version("junk"), None);
        assert_eq!(unix_minor("libpython3.12.so.1.0"), Some(12));
        assert_eq!(unix_minor("libpython3.9.so"), Some(9));
        assert_eq!(unix_minor("libpython3.12d.so"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_skips_old_and_incomplete_pythons() {
        let root = scratch("win");
        let old = root.join("Python310");
        touch(&old.join("python3.dll"));
        touch(&old.join("python310.dll"));
        touch(&old.join("Lib/encodings/__init__.py"));
        let bare = root.join("bare");
        touch(&bare.join("python3.dll"));
        touch(&bare.join("python312.dll"));
        let good = root.join("Python312");
        touch(&good.join("python3.dll"));
        touch(&good.join("python312.dll"));
        touch(&good.join("Lib/encodings/__init__.py"));
        let embed = root.join("Python313");
        touch(&embed.join("python3.dll"));
        touch(&embed.join("python313.dll"));
        touch(&embed.join("python313.zip"));

        // PATH order: the old one first, it is skipped.
        let env = Env { path: vec![old.clone(), bare.clone(), good.clone()], ..Env::default() };
        let found = find(&env).unwrap();
        assert_eq!((found.library, found.minor, found.source), (good.join("python3.dll"), 12, "PATH"));

        // Default places: the newest (here the embeddable layout).
        let env = Env { defaults: vec![root.to_path_buf()], ..Env::default() };
        assert_eq!(find(&env).unwrap().minor, 13);

        // Only old ones: a clean message naming them.
        let env = Env { path: vec![old.clone(), bare.clone()], ..Env::default() };
        let e = find(&env).unwrap_err();
        assert!(
            e.contains("3.12 or newer") && e.contains("is Python 3.10") && e.contains("no standard library"),
            "{e}"
        );

        // An explicit setting is taken or refused, never silently replaced.
        let env = Env { explicit: Some(old.clone()), path: vec![good.clone()], ..Env::default() };
        assert!(find(&env).unwrap_err().starts_with("PENUMBRA_PYTHON="));
        let env = Env { explicit: Some(good.join("python312.dll")), ..Env::default() };
        assert_eq!(find(&env).unwrap().source, "PENUMBRA_PYTHON");

        // PYTHONHOME elsewhere would abort Py_Initialize.
        let env = Env { path: vec![good.clone()], python_home: Some(old.clone()), ..Env::default() };
        assert!(find(&env).unwrap_err().contains("PYTHONHOME"));
        let env = Env { path: vec![good.clone()], python_home: Some(good.clone()), ..Env::default() };
        assert!(find(&env).is_ok());

        assert!(find(&Env::default()).unwrap_err().contains("no Python 3.12"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_finds_the_newest_libpython() {
        let root = scratch("unix");
        touch(&root.join("libpython3.11.so.1.0"));
        touch(&root.join("libpython3.12.so.1.0"));
        let env = Env { defaults: vec![root.to_path_buf()], ..Env::default() };
        let found = find(&env).unwrap();
        assert_eq!((found.library, found.minor), (root.join("libpython3.12.so.1.0"), 12));
        let env = Env { explicit: Some(root.join("libpython3.11.so.1.0")), ..Env::default() };
        assert!(find(&env).unwrap_err().contains("is Python 3.11"));
    }
}
