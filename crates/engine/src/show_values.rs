//! The values of the show screen's controls, kept across a crash. In show mode a toggle button
//! or a slider on the show screen may change ([`crate::buttons::set_in_show`]), but nothing is
//! written into `project.ron` until show mode ends. When the app crashes or closes before that,
//! the next start would open the values of the file again, in the middle of a show.
//!
//! So while the project is locked, the [`Keeper`] writes those values into [`FILE`] in
//! penumbra's data directory, beside the show mode file, whenever they changed (looked at on
//! every poll of the project thread, a few times per second, and once more when a crash is
//! recorded or the project closes). The project folder stays untouched, as show mode promises.
//! The next start with show mode on sets them again ([`restore`]), as live values without
//! history. The file names the project and the size and hash of its `project.ron`, so values
//! are only taken for the same project and the same file: a file edited meanwhile, another
//! project or a file that does not parse are ignored. Once show mode has ended and the project
//! file holds the values, the file is deleted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use penumbra_core::{Error, Name, NodePath, ParamPath, Value, lock};
use penumbra_graph::Op;
use penumbra_project::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// The file's name, beside [`crate::show::FILE`].
pub const FILE: &str = "show-values.json";

/// A larger file is not read: the values of a show screen take a few hundred bytes.
const MAX_BYTES: u64 = 1 << 20;

/// More values than this are not taken: no show screen has that many controls.
const MAX_VALUES: usize = 10_000;

/// What the file holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kept {
    /// The project directory the values belong to.
    pub project: String,
    /// `project.ron` when the values were taken: its size in bytes and FNV-1a hash (hex).
    pub file_bytes: u64,
    pub file_hash: String,
    /// `value` by node path: true or false for a toggle button, a number for a slider.
    pub values: BTreeMap<String, Json>,
}

/// Size and hash of a project file's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    hash: String,
}

fn stamp_of(file: &Path) -> Result<Stamp, Error> {
    let bytes = std::fs::read(file).map_err(|e| Error::new(format!("{}: {e}", file.display())))?;
    // FNV-1a, 64 bit: the same in every build, unlike the standard library's hasher.
    let hash = bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    Ok(Stamp { bytes: bytes.len() as u64, hash: format!("{hash:016x}") })
}

/// The values as the file writes them.
fn to_json(values: &BTreeMap<String, Value>) -> BTreeMap<String, Json> {
    values
        .iter()
        .filter_map(|(path, v)| {
            let json = match v {
                Value::Bool(b) => Json::Bool(*b),
                Value::Float(x) => serde_json::Number::from_f64(*x).map(Json::Number)?,
                Value::Int(i) => Json::from(*i),
                _ => return None,
            };
            Some((path.clone(), json))
        })
        .collect()
}

/// The last write, so an unchanged set of values is not written again.
struct Written {
    dir: PathBuf,
    stamp: Stamp,
    values: BTreeMap<String, Json>,
}

#[derive(Default)]
struct State {
    written: Option<Written>,
    /// The file exists (as far as this process knows).
    on_disk: bool,
    /// A failure was logged; the next is not, until a write works again.
    warned: bool,
}

/// Keeps [`FILE`] in step with the open project while it is locked for a show.
#[derive(Default)]
pub struct Keeper {
    file: Mutex<Option<PathBuf>>,
    state: Mutex<State>,
}

impl Keeper {
    /// Where the values are kept; `None` keeps nothing (headless runs, tests).
    pub fn set_file(&self, file: Option<PathBuf>) {
        let on_disk = file.as_deref().is_some_and(Path::exists);
        *lock(&self.file) = file;
        *lock(&self.state) = State { written: None, on_disk, warned: false };
    }

    pub fn file(&self) -> Option<PathBuf> {
        lock(&self.file).clone()
    }

    /// Brings the file in step with `session`: while it is locked, writes the values when they
    /// changed; once it is unlocked and its file holds them, deletes the file. Cheap when
    /// nothing changed (one look at the document). Errors are logged; a show goes on without.
    pub fn tick(&self, session: &Session) {
        let Some(file) = self.file() else { return };
        let mut state = lock(&self.state);
        if !session.locked() {
            state.written = None;
            if state.on_disk && !session.status(Instant::now()).unsaved {
                match std::fs::remove_file(&file) {
                    Ok(()) => log::info!(
                        "show mode has ended and the project file holds the values; {FILE} deleted"
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => log::warn!("cannot delete {}: {e}", file.display()),
                }
                state.on_disk = false;
            }
            return;
        }
        let values = to_json(&crate::buttons::kept_values(&session.doc().snapshot(), session.types()));
        let same = state.written.as_ref().filter(|w| w.dir == session.dir());
        if same.is_some_and(|w| w.values == values) || (values.is_empty() && !state.on_disk) {
            return;
        }
        let stamp = match same {
            Some(w) => w.stamp.clone(),
            None => match stamp_of(session.file()) {
                Ok(stamp) => stamp,
                Err(e) => {
                    warn_once(&mut state, &e);
                    return;
                }
            },
        };
        let kept = Kept {
            project: session.dir().display().to_string(),
            file_bytes: stamp.bytes,
            file_hash: stamp.hash.clone(),
            values: values.clone(),
        };
        let written = serde_json::to_string_pretty(&kept)
            .map_err(|e| Error::new(e.to_string()))
            .and_then(|text| crate::show::write_atomic(&file, &text));
        match written {
            Ok(()) => {
                state.on_disk = true;
                state.warned = false;
                state.written = Some(Written { dir: session.dir().to_path_buf(), stamp, values });
            }
            Err(e) => warn_once(&mut state, &e),
        }
    }
}

fn warn_once(state: &mut State, e: &Error) {
    if !state.warned {
        log::warn!("the show screen's values are not kept across a crash: {e}");
        state.warned = true;
    }
}

/// The file's content, if it belongs to `session` as it is on disk now; `None` (and why) when
/// it is missing, too large, does not parse, names another project or a different
/// `project.ron`.
pub fn read(file: &Path, session: &Session) -> Result<Kept, String> {
    let size = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if size > MAX_BYTES {
        return Err(format!("{} is {size} bytes, more than a show screen's values take", file.display()));
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let kept: Kept =
        serde_json::from_str(&text).map_err(|e| format!("{} does not parse: {e}", file.display()))?;
    if kept.project != session.dir().display().to_string() {
        return Err(format!("{} belongs to {}, not to the open project", file.display(), kept.project));
    }
    let stamp = stamp_of(session.file()).map_err(|e| e.0)?;
    if kept.file_bytes != stamp.bytes || kept.file_hash != stamp.hash {
        return Err(format!("{} has changed since the values were kept", session.file().display()));
    }
    if kept.values.len() > MAX_VALUES {
        return Err(format!(
            "{} lists {} values, more than a show screen has",
            file.display(),
            kept.values.len()
        ));
    }
    Ok(kept)
}

/// Sets the values kept in `file` again in the locked `session`, as the show screen would:
/// only the `value` of toggle buttons and sliders that are still on the show screen, each
/// checked like a param edit. Returns how many were set; a value that does not fit any more
/// (node gone, unflagged, bound, wrong type) is skipped.
pub fn restore(file: &Path, session: &Session) -> Result<usize, String> {
    if !session.locked() {
        return Err("the project is not locked for a show".into());
    }
    let kept = read(file, session)?;
    let mut set = 0;
    for (path, json) in &kept.values {
        let value = match json {
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => match n.as_f64().filter(|x| x.is_finite()) {
                Some(x) => Value::Float(x),
                None => continue,
            },
            _ => continue,
        };
        let Ok(node) = path.parse::<NodePath>() else { continue };
        let Some(param) = Name::new("value").ok().and_then(|n| ParamPath::new(node, n).ok()) else {
            continue;
        };
        let ops = [Op::Set { param, value }];
        match crate::buttons::set_in_show(session, &ops) {
            Ok(true) => set += 1,
            Ok(false) => {
                log::info!("{path}: no longer a control on the show screen; its kept value is skipped")
            }
            Err(e) => log::info!("{path}: kept value skipped: {e}"),
        }
    }
    Ok(set)
}
