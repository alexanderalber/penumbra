//! What a show has changed, kept across a crash. In show mode a toggle button or a slider on
//! the show screen may change ([`crate::buttons::set_in_show`]), OSC and HTTP set params by
//! address ([`crate::osc`]), and a cue list moves on, but nothing is written into `project.ron`
//! until show mode ends. When the app crashes or closes before that, the next start would open
//! the values of the file again and start every cue list at its first cue, in the middle of a
//! show. A desk usually sends a fader only when it moves, so its value would not come back by
//! itself either.
//!
//! So while the project is locked, the [`Keeper`] writes into [`FILE`] in penumbra's data
//! directory, beside the show mode file: the show screen's values, the params set live by
//! address, and the place of every node that keeps one ([`penumbra_graph::Operator::show_place`],
//! a `chan.sequence`'s cue). It writes whenever one of them changed (looked at on every poll of
//! the project thread, a few times per second, and once more when a crash is recorded or the
//! project closes). The project folder stays untouched, as show mode promises. The next start
//! with show mode on sets them again ([`restore`]), as live values without history, and hands
//! the places back to their nodes. The file names the project and the size and hash of its
//! `project.ron`, so it is only taken for the same project and the same file: a file edited
//! meanwhile, another project or a file that does not parse are ignored. Each value is checked
//! again like an edit, and a place goes only to a node of the kind that kept it. Once show mode
//! has ended and the project file holds the values, the file is deleted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use penumbra_core::{Error, Name, NodeId, NodePath, ParamPath, Value, lock};
use penumbra_graph::{Network, NodeTypes, Op};
use penumbra_project::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// The file's name, beside [`crate::show::FILE`].
pub const FILE: &str = "show-values.json";

/// A larger file is not read: the values of a show take a few kilobytes.
const MAX_BYTES: u64 = 1 << 20;

/// More values (or places) than this are not taken: no show has that many controls.
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
    /// Params set live by address (OSC, HTTP) by param path (`/node.param`), apart from the
    /// show screen's values.
    pub live: BTreeMap<String, Json>,
    /// Where nodes stand in the show, by node path.
    pub places: BTreeMap<String, KeptPlace>,
}

/// A node's place in the show, as its operator wrote it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeptPlace {
    /// The node's kind: the place goes back only to a node of this kind.
    pub kind: String,
    /// The operator's own encoding (JSON for a cue list).
    pub place: Json,
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

/// The params set live in `net` (`live`, from [`penumbra_graph::Document::live_params`]) with
/// their values now, as the file writes them. The show screen's values (`controls`, by node
/// path) are left out: they are kept on their own.
fn live_json(
    net: &Network,
    types: &NodeTypes,
    live: &[(NodeId, Name)],
    controls: &BTreeMap<String, Json>,
) -> BTreeMap<String, Json> {
    let mut out = BTreeMap::new();
    for (id, name) in live {
        if out.len() >= MAX_VALUES {
            break;
        }
        let (Some(node), Some(path)) = (net.node(*id), net.path_of(*id)) else { continue };
        let path = path.to_string();
        if name.as_str() == "value" && controls.contains_key(&path) {
            continue;
        }
        let Some(desc) = types.desc(&node.kind) else { continue };
        let value = node
            .param(desc, name.as_str())
            .cloned()
            .or_else(|| desc.param_spec(name.as_str()).map(|s| s.default.clone()));
        // A value JSON cannot hold (not finite) is not kept.
        if let Some(json) = value.and_then(|v| serde_json::to_value(&v).ok()) {
            out.insert(format!("{path}.{name}"), json);
        }
    }
    out
}

/// The places `places` lists, by node path, with their node's kind.
fn places_json(net: &Network, places: &[(NodeId, String)]) -> BTreeMap<String, KeptPlace> {
    places
        .iter()
        .take(MAX_VALUES)
        .filter_map(|(id, place)| {
            let (node, path) = (net.node(*id)?, net.path_of(*id)?);
            let place = serde_json::from_str(place).unwrap_or_else(|_| Json::String(place.clone()));
            Some((path.to_string(), KeptPlace { kind: node.kind.clone(), place }))
        })
        .collect()
}

/// What the file holds besides the project's stamp.
#[derive(Clone, Debug, Default, PartialEq)]
struct Content {
    values: BTreeMap<String, Json>,
    live: BTreeMap<String, Json>,
    places: BTreeMap<String, KeptPlace>,
}

impl Content {
    fn is_empty(&self) -> bool {
        self.values.is_empty() && self.live.is_empty() && self.places.is_empty()
    }
}

/// The last write, so unchanged content is not written again.
struct Written {
    dir: PathBuf,
    stamp: Stamp,
    content: Content,
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
    /// The places the cook thread handed over last ([`Keeper::set_places`]).
    places: Mutex<Vec<(NodeId, String)>>,
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

    /// The places of the nodes that keep one, as the cook thread collected them
    /// ([`penumbra_graph::Cooker::show_places`]); the next [`Keeper::tick`] writes them.
    pub fn set_places(&self, places: Vec<(NodeId, String)>) {
        *lock(&self.places) = places;
    }

    /// Brings the file in step with `session`: while it is locked, writes the values and places
    /// when they changed; once it is unlocked and its file holds the values, deletes the file.
    /// Cheap when nothing changed (one look at the document). Errors are logged; a show goes on
    /// without.
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
        let net = session.doc().snapshot();
        let values = to_json(&crate::buttons::kept_values(&net, session.types()));
        let live = live_json(&net, session.types(), &session.doc().live_params(), &values);
        let places = places_json(&net, &lock(&self.places));
        let content = Content { values, live, places };
        let same = state.written.as_ref().filter(|w| w.dir == session.dir());
        if same.is_some_and(|w| w.content == content) || (content.is_empty() && !state.on_disk) {
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
            values: content.values.clone(),
            live: content.live.clone(),
            places: content.places.clone(),
        };
        let written = serde_json::to_string_pretty(&kept)
            .map_err(|e| Error::new(e.to_string()))
            .and_then(|text| crate::show::write_atomic(&file, &text));
        match written {
            Ok(()) => {
                state.on_disk = true;
                state.warned = false;
                state.written = Some(Written { dir: session.dir().to_path_buf(), stamp, content });
            }
            Err(e) => warn_once(&mut state, &e),
        }
    }
}

fn warn_once(state: &mut State, e: &Error) {
    if !state.warned {
        log::warn!("the show's values and cues are not kept across a crash: {e}");
        state.warned = true;
    }
}

/// The file's content, if it belongs to `session` as it is on disk now; `None` (and why) when
/// it is missing, too large, does not parse, names another project or a different
/// `project.ron`.
pub fn read(file: &Path, session: &Session) -> Result<Kept, String> {
    let size = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if size > MAX_BYTES {
        return Err(format!("{} is {size} bytes, more than a show's values take", file.display()));
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
    let most = kept.values.len().max(kept.live.len()).max(kept.places.len());
    if most > MAX_VALUES {
        return Err(format!("{} lists {most} values, more than a show has", file.display()));
    }
    Ok(kept)
}

/// What [`restore`] set again and what it hands on.
#[derive(Debug, Default)]
pub struct Restored {
    /// Show screen values set.
    pub values: usize,
    /// Params set by address set again.
    pub live: usize,
    /// Places for the cook thread to give back ([`penumbra_graph::Cooker::restore_show_place`]),
    /// each for a node of the kind that kept it.
    pub places: Vec<(NodeId, String)>,
}

/// Sets the values kept in `file` again in the locked `session`: the show screen's as the show
/// screen would (only the `value` of toggle buttons and sliders still on it), the params set
/// by address as OSC would (not on locked nodes or bound params), each checked like a param
/// edit. A value that does not fit any more (node gone, unflagged, bound, wrong type) is
/// skipped. The places are returned for the nodes that are still of the kind that kept them.
pub fn restore(file: &Path, session: &Session) -> Result<Restored, String> {
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
    let mut live = 0;
    for (path, json) in &kept.live {
        match restore_live(session, path, json) {
            Ok(()) => live += 1,
            Err(e) => log::info!("{path}: kept value skipped: {e}"),
        }
    }
    let net = session.doc().snapshot();
    let mut places = Vec::new();
    for (path, kept) in &kept.places {
        let node = path.parse::<NodePath>().ok().and_then(|p| net.resolve(&p)).and_then(|id| net.node(id));
        match node {
            Some(node) if node.kind == kept.kind => {
                let place = match &kept.place {
                    Json::String(text) => text.clone(),
                    other => other.to_string(),
                };
                places.push((node.id, place));
            }
            Some(node) => {
                log::info!("{path}: is a {} now, not a {}; its kept place is skipped", node.kind, kept.kind)
            }
            None => log::info!("{path}: no such node any more; its kept place is skipped"),
        }
    }
    Ok(Restored { values: set, live, places })
}

/// Sets one param kept by address (`/node.param`) again, if it still takes a live value: the
/// node exists and is not locked, its kind declares the param, the param is not bound, and the
/// value fits it.
fn restore_live(session: &Session, path: &str, json: &Json) -> Result<(), String> {
    let param: ParamPath = path.parse().map_err(|e: Error| e.0)?;
    let net = session.doc().snapshot();
    let node = net.resolve(param.node()).and_then(|id| net.node(id)).ok_or("no such node any more")?;
    if node.locked {
        return Err("the node is locked".into());
    }
    let name = param.param().as_str();
    if node.binds.keys().any(|p| p.param.as_str() == name) {
        return Err("bound to a channel now".into());
    }
    let types = session.types();
    let desc = types.desc(&node.kind).ok_or_else(|| format!("unknown node kind `{}`", node.kind))?;
    let spec = desc.param_spec(name).ok_or_else(|| format!("`{}` has no param `{name}`", node.kind))?;
    if name == "value" && crate::buttons::on_show_screen(types, node) {
        return Err("a control on the show screen; its value is kept with the show screen's".into());
    }
    let value: Value = serde_json::from_value(json.clone()).map_err(|e| format!("not a param value: {e}"))?;
    let value = spec.check(&value).map_err(|e| e.0)?;
    session.edit_locked(&[Op::Set { param, value }]).map_err(|e| e.0)?;
    Ok(())
}
