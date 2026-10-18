//! The sources on the network, watched by one thread for the whole process: started by the
//! first question (a menu, `ndi.sources`, a receiver), kept until the process ends, so a menu
//! opened later lists at once what is there.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use penumbra_core::lock;

use crate::ffi::{self, Finder, Source};

/// How long after the start the list may still be filling (sources answer within about half a
/// second on a local network).
const SETTLING: Duration = Duration::from_millis(1500);

struct Directory {
    sources: Mutex<Vec<Source>>,
    started: Instant,
}

static DIRECTORY: OnceLock<Result<Arc<Directory>, String>> = OnceLock::new();

fn directory() -> Result<&'static Arc<Directory>, String> {
    DIRECTORY.get_or_init(start).as_ref().map_err(Clone::clone)
}

fn start() -> Result<Arc<Directory>, String> {
    let ndi = ffi::runtime()?;
    let finder = Finder::new(ndi)?;
    let dir = Arc::new(Directory { sources: Mutex::new(Vec::new()), started: Instant::now() });
    let shared = dir.clone();
    std::thread::Builder::new()
        .name("ndi sources".into())
        .spawn(move || {
            loop {
                finder.wait(1000);
                let mut list = finder.sources();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                list.dedup_by(|a, b| a.name == b.name);
                *lock(&shared.sources) = list;
            }
        })
        .map_err(|e| format!("cannot start looking for NDI sources: {e}"))?;
    Ok(dir)
}

/// The sources seen now, sorted by name; starts looking on the first call. An error when the
/// runtime is missing.
pub fn sources() -> Result<Vec<Source>, String> {
    Ok(lock(&directory()?.sources).clone())
}

/// Whether the list may still be filling: looking started only just now.
pub fn settling() -> bool {
    directory().is_ok_and(|d| d.started.elapsed() < SETTLING)
}

/// The source `wanted` names: its full name, else the first whose name contains it (case
/// ignored); empty wants the first source.
pub fn find(wanted: &str) -> Result<Option<Source>, String> {
    let list = sources()?;
    Ok(pick(&list, wanted))
}

pub(crate) fn pick(list: &[Source], wanted: &str) -> Option<Source> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return list.first().cloned();
    }
    if let Some(s) = list.iter().find(|s| s.name == wanted) {
        return Some(s.clone());
    }
    let lower = wanted.to_lowercase();
    list.iter().find(|s| s.name.to_lowercase().contains(&lower)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_found_by_its_name_or_a_part() {
        let s = |n: &str| Source { name: n.into(), url: String::new() };
        let list = vec![s("A (Test Patterns)"), s("B (OBS)"), s("B (OBS 2)")];
        assert_eq!(pick(&list, "").map(|s| s.name), Some("A (Test Patterns)".into()));
        assert_eq!(pick(&list, "B (OBS 2)").map(|s| s.name), Some("B (OBS 2)".into()));
        assert_eq!(pick(&list, "obs").map(|s| s.name), Some("B (OBS)".into()));
        assert_eq!(pick(&list, " test ").map(|s| s.name), Some("A (Test Patterns)".into()));
        assert_eq!(pick(&list, "resolume"), None);
        assert_eq!(pick(&[], ""), None);
    }
}
