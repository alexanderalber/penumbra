//! `comp.*`: grouping nodes into components and back, component files and the library. The content
//! work is in `penumbra_project::component`; here it becomes one undoable edit each.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_core::{Name, NodeId, NodePath};
use penumbra_graph::{EditOutcome, Network, NodeTypes, Op, PortRole};
use penumbra_ops::comp::COMP_KIND;
use penumbra_project::component::{self, FILE_SUFFIX};
use penumbra_project::{Actor, Content, diff};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{edit, engine_commands, outcome, require};
use crate::engine::Engine;

type Result<T> = std::result::Result<T, CommandError>;

/// Largest component file read.
const MAX_FILE_BYTES: u64 = penumbra_project::file::MAX_FILE_BYTES;
/// Directory inside a project for its own components.
pub const PROJECT_DIR: &str = "components";

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "comp.collapse",
        "Group side-by-side nodes into a new component, ports where wires cross; {paths, name?}",
        comp_collapse,
    );
    add("comp.expand", "Dissolve a component, its nodes move up beside it; {path}", comp_expand);
    add(
        "comp.save",
        "Write a component to a file (default: the library, <name>.comp.ron); {path, file?}",
        comp_save,
    );
    add(
        "comp.load",
        "Add a component from a file or a library (by name); one in a folder of its own copies that folder's files into the project's library/<name>/ first; {file, path?}",
        comp_load,
    );
    add(
        "comp.library",
        "Component files in the project's components/, the user's library and penumbra's own library",
        comp_library,
    );
    add(
        "comp.remove",
        "Take a component file out of the library (into its removed/ folder); {name, place?: project | library}",
        comp_remove,
    );
}

/// Where [`remove`] puts a component file, inside the directory it came from. Not listed, so
/// out of the add dialog; a file can be moved back by hand.
pub const REMOVED_DIR: &str = "removed";

/// Takes the component file `name` out of the library: the project's first, as `comp.load`
/// picks it, unless `place` says which. Moved into [`REMOVED_DIR`] beside it (an earlier one
/// of the same name there is replaced), not deleted. Returns where it went.
pub fn remove(e: &Engine, name: &str, place: Option<&str>) -> Result<PathBuf> {
    let entry = library(e)
        .into_iter()
        .find(|x| x.name == name && place.is_none_or(|p| p == x.place))
        .ok_or_else(|| format!("no component file `{name}`"))?;
    if entry.place == "penumbra" {
        return Err(format!("`{name}` ships with penumbra and stays in its library").into());
    }
    let dir = entry.file.parent().ok_or("the component file has no directory")?.join(REMOVED_DIR);
    std::fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let file_name = entry.file.file_name().ok_or("the component file has no name")?;
    let to = dir.join(file_name);
    std::fs::rename(&entry.file, &to)
        .map_err(|err| format!("cannot move {}: {err}", entry.file.display()))?;
    Ok(to)
}

/// The user's library when nothing else is set: `PENUMBRA_LIBRARY`, else `components` in the
/// app's data directory.
pub fn default_library() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("PENUMBRA_LIBRARY") {
        return Some(dir);
    }
    crate::projects::data_dir().ok().map(|d| d.join("components"))
}

/// One component file found.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct LibraryEntry {
    /// The file name without `.comp.ron`.
    pub name: String,
    pub file: PathBuf,
    /// `project`, `library` or `penumbra` (shipped with penumbra).
    pub place: &'static str,
}

/// Where the shipped library is looked for: as many directories above the executable's own as
/// the examples (the release zip has it beside the executable, a checkout above `target/`).
const SHIPPED_SEARCH_DEPTH: usize = 4;
/// The shipped library's directory name.
pub const SHIPPED_DIR: &str = "library";
/// Where a component's own files go in a project: `library/<name>/`, the same path its nodes
/// name them by, so a component in a folder of its own works wherever it is loaded.
pub const FILES_DIR: &str = "library";
/// Most files a component brings along, and the largest one.
const MAX_FILES: usize = 64;
const MAX_FILE_COPY_BYTES: u64 = 64 << 20;

/// The component library shipped with penumbra: a `library` directory beside the executable or
/// a few levels above it holding at least one component file.
pub fn shipped_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(SHIPPED_SEARCH_DEPTH + 1)
        .map(|d| d.join(SHIPPED_DIR))
        .find(|d| !files_in(d, "penumbra").is_empty())
}

/// The component files in `dir`: `<name>.comp.ron` directly in it, or in a folder of its own,
/// `<name>/<name>.comp.ron` (whose other files travel with it, see [`load`]); sorted by name.
fn files_in(dir: &Path, place: &'static str) -> Vec<LibraryEntry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<LibraryEntry> = read
        .flatten()
        .take(10_000)
        .filter_map(|entry| {
            let path = entry.path();
            let kind = entry.file_type().ok()?;
            let (name, file) = if kind.is_dir() {
                let name = path.file_name()?.to_str()?.to_string();
                if name == REMOVED_DIR {
                    return None;
                }
                let file = path.join(format!("{name}{FILE_SUFFIX}"));
                (name, file)
            } else {
                (path.file_name()?.to_str()?.strip_suffix(FILE_SUFFIX)?.to_string(), path)
            };
            (!name.is_empty() && file.is_file()).then_some(LibraryEntry { name, file, place })
        })
        .collect();
    found.sort_by(|a, b| penumbra_graph::natural_cmp(&a.name, &b.name));
    found
}

/// The component files of the project, the user's library and penumbra's own, in that order,
/// each sorted by name. Unreadable directories count as empty.
pub fn library(e: &Engine) -> Vec<LibraryEntry> {
    let dirs = [
        (Some(e.session().dir().join(PROJECT_DIR)), "project"),
        (e.library(), "library"),
        (e.shipped_library(), "penumbra"),
    ];
    dirs.into_iter().filter_map(|(dir, place)| Some(files_in(&dir?, place))).flatten().collect()
}

fn history(out: &penumbra_graph::EditOutcome) -> Json {
    outcome(out).get("history").cloned().unwrap_or(Json::Null)
}

fn content(net: &Network) -> Result<Content> {
    Ok(Content::from_network(net)?)
}

fn moved_json(moved: &BTreeMap<NodePath, NodePath>) -> Json {
    moved.iter().map(|(a, b)| (a.to_string(), json!(b))).collect::<serde_json::Map<_, _>>().into()
}

/// A free name for a new node under `parent`: `base1`, `base2`, ... (`base` itself if it ends in
/// a digit and is free).
fn fresh_child(net: &Network, parent: &NodePath, base: &str) -> Result<NodePath> {
    let parent_id = if parent.is_root() { None } else { Some(require(net, parent)?) };
    let taken: Vec<&str> = net.children(parent_id).map(|n| n.name.as_str()).collect();
    if base.ends_with(|c: char| c.is_ascii_digit()) && !taken.contains(&base) {
        return Ok(parent.child(Name::new(base)?)?);
    }
    for i in 1..100_000 {
        let n = format!("{base}{i}");
        if !taken.contains(&n.as_str()) {
            return Ok(parent.child(Name::new(&n)?)?);
        }
    }
    Err(format!("no free name for `{base}` in {parent}").into())
}

pub(crate) fn edit_as(e: &Engine, actor: Actor, ops: &[Op], label: &str) -> Result<EditOutcome> {
    if actor == Actor::Claude {
        return edit(e, ops, label);
    }
    Ok(e.session().edit(actor, ops, None, label, Instant::now())?)
}

/// What grouping did.
pub struct Grouped {
    pub path: NodePath,
    /// Old path to new path of every moved node.
    pub moved: BTreeMap<NodePath, NodePath>,
    pub ports: Vec<NodePath>,
    pub outcome: EditOutcome,
}

/// Groups `paths` (side by side) into a new component beside them, as `actor`.
pub fn collapse(e: &Engine, actor: Actor, paths: &[NodePath], name: Option<Name>) -> Result<Grouped> {
    let first = paths.first().ok_or("nothing to group")?;
    let level = first.parent().ok_or("the root cannot be grouped")?;
    let net = e.session().doc().snapshot();
    let at = match name {
        Some(n) => level.child(n)?,
        None => fresh_child(&net, &level, "group")?,
    };
    let base = content(&net)?;
    let types = e.types();
    let c = component::collapse(&base, types, paths, &at, COMP_KIND)?;
    let ops = diff(&base, &c.content);
    let outcome = edit_as(e, actor, &ops, &format!("group into {at}"))?;
    Ok(Grouped { path: at, moved: c.moved, ports: c.ports, outcome })
}

/// Dissolves the component at `path`, as `actor`; old path to new path of every moved node.
pub fn expand(
    e: &Engine,
    actor: Actor,
    path: &NodePath,
) -> Result<(BTreeMap<NodePath, NodePath>, EditOutcome)> {
    let net = e.session().doc().snapshot();
    let base = content(&net)?;
    let x = component::expand(&base, e.types(), path)?;
    let ops = diff(&base, &x.content);
    let out = edit_as(e, actor, &ops, &format!("dissolve {path}"))?;
    Ok((x.moved, out))
}

fn comp_collapse(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        paths: Vec<NodePath>,
        name: Option<Name>,
    }
    let a: A = args(p)?;
    let g = collapse(e, Actor::Claude, &a.paths, a.name)?;
    Ok(json!({
        "path": g.path,
        "moved": moved_json(&g.moved),
        "ports": g.ports,
        "history": history(&g.outcome),
    }))
}

fn comp_expand(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
    }
    let a: A = args(p)?;
    let (moved, out) = expand(e, Actor::Claude, &a.path)?;
    Ok(json!({"moved": moved_json(&moved), "history": history(&out)}))
}

/// `file` as given: absolute, or relative to the project directory.
fn in_project(e: &Engine, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() { path.to_path_buf() } else { e.session().dir().join(path) }
}

/// What saving a component did.
pub struct Saved {
    pub file: PathBuf,
    pub replaced: bool,
    /// References to the outside the file could not carry.
    pub dropped: Vec<String>,
}

/// Writes the component at `path` to `file` (absolute or in the project; default: the library).
pub fn save(e: &Engine, path: &NodePath, file: Option<&str>) -> Result<Saved> {
    let net = e.session().doc().snapshot();
    let types = e.types();
    let (comp, dropped) = component::extract(&content(&net)?, types, path)?;
    let text = component::print(&comp, types)?;
    let name = path.name().ok_or("the root is not a component")?;
    let file = match file {
        Some(f) => {
            let f = in_project(e, f);
            if f.to_string_lossy().ends_with(FILE_SUFFIX) { f } else { f.with_extension("comp.ron") }
        }
        None => e.library().ok_or("no library directory; pass `file`")?.join(format!("{name}{FILE_SUFFIX}")),
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    }
    let replaced = file.exists();
    let tmp = file.with_extension("ron.tmp");
    std::fs::write(&tmp, &text).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|err| format!("cannot write {}: {err}", file.display()))?;
    Ok(Saved { file, replaced, dropped: dropped.lines })
}

fn comp_save(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        file: Option<String>,
    }
    let a: A = args(p)?;
    let s = save(e, &a.path, a.file.as_deref())?;
    Ok(json!({"file": s.file.display().to_string(), "replaced": s.replaced, "dropped": s.dropped}))
}

/// The file `name` stands for: a path (absolute or in the project), else a component of that
/// name in the project's `components/` or the library.
fn find_component(e: &Engine, name: &str) -> Result<PathBuf> {
    let direct = in_project(e, name);
    if direct.is_file() {
        return Ok(direct);
    }
    let bare = name.strip_suffix(FILE_SUFFIX).unwrap_or(name);
    library(e).into_iter().find(|entry| entry.name == bare).map(|entry| entry.file).ok_or_else(|| {
        let known: Vec<String> = library(e).into_iter().map(|x| x.name).collect();
        format!("no component file `{name}`; in the library: {}", known.join(", ")).into()
    })
}

/// What loading a component did.
pub struct Loaded {
    pub path: NodePath,
    pub outcome: EditOutcome,
    /// Files the component brought along, copied into the project (relative to it).
    pub copied: Vec<String>,
    /// Files it brings that the project had already; those are left as they are.
    pub kept: Vec<String>,
}

/// The files a component in a folder of its own (`<name>/<name>.comp.ron`) brings along: the
/// other files of that folder, except its preview picture and notes. Copied into the project's
/// `library/<name>/` unless already there (a changed copy in the project wins). Returns the
/// copied and the kept ones, relative to the project.
fn bring_files(e: &Engine, file: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let (mut copied, mut kept) = (Vec::new(), Vec::new());
    let Some(dir) = file.parent() else { return Ok((copied, kept)) };
    let Some(name) = file.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(FILE_SUFFIX))
    else {
        return Ok((copied, kept));
    };
    if dir.file_name().and_then(|n| n.to_str()) != Some(name) {
        return Ok((copied, kept));
    }
    let to = e.session().dir().join(FILES_DIR).join(name);
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same(dir, &to) {
        return Ok((copied, kept));
    }
    let read = std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    let mut files: Vec<PathBuf> = read
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !n.ends_with(FILE_SUFFIX) && !n.starts_with("preview.") && !n.ends_with(".md")
        })
        .take(MAX_FILES + 1)
        .collect();
    if files.len() > MAX_FILES {
        return Err(format!("{} holds more than {MAX_FILES} files to bring along", dir.display()).into());
    }
    files.sort();
    for from in files {
        let Some(file_name) = from.file_name() else { continue };
        let target = to.join(file_name);
        let shown = format!("{FILES_DIR}/{name}/{}", file_name.to_string_lossy());
        if target.exists() {
            kept.push(shown);
            continue;
        }
        let size =
            std::fs::metadata(&from).map_err(|err| format!("cannot read {}: {err}", from.display()))?.len();
        if size > MAX_FILE_COPY_BYTES {
            return Err(format!("{} is larger than {} MiB", from.display(), MAX_FILE_COPY_BYTES >> 20).into());
        }
        std::fs::create_dir_all(&to).map_err(|err| format!("cannot create {}: {err}", to.display()))?;
        std::fs::copy(&from, &target).map_err(|err| format!("cannot copy {}: {err}", from.display()))?;
        copied.push(shown);
    }
    Ok((copied, kept))
}

/// Reads a component file and adds it at `path` (default: in `level`, named after it), as
/// `actor`. A component in a folder of its own brings that folder's files into the project
/// first ([`bring_files`]).
pub fn load(
    e: &Engine,
    actor: Actor,
    file: &str,
    path: Option<NodePath>,
    level: &NodePath,
) -> Result<Loaded> {
    let file = find_component(e, file)?;
    let size =
        std::fs::metadata(&file).map_err(|err| format!("cannot read {}: {err}", file.display()))?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("{} is larger than {} MiB", file.display(), MAX_FILE_BYTES >> 20).into());
    }
    let text =
        std::fs::read_to_string(&file).map_err(|err| format!("cannot read {}: {err}", file.display()))?;
    let types: &NodeTypes = e.types();
    let comp = component::parse(&text, types).map_err(|err| err.context(file.display().to_string()))?;
    let top = comp.nodes.keys().find(|k| k.names().len() == 1).and_then(|k| k.name()).cloned();
    let net = e.session().doc().snapshot();
    let at = match path {
        Some(p) => p,
        None => {
            let base = top.map(|n| n.to_string()).unwrap_or_else(|| "comp".into());
            match net.resolve(&level.child(Name::new(&base)?)?) {
                None => level.child(Name::new(&base)?)?,
                Some(_) => fresh_child(&net, level, base.trim_end_matches(|c: char| c.is_ascii_digit()))?,
            }
        }
    };
    let base = content(&net)?;
    let next = component::instantiate(&base, types, &comp, &at)?;
    let (copied, kept) = bring_files(e, &file)?;
    let ops = diff(&base, &next);
    let outcome = edit_as(e, actor, &ops, &format!("add component {at}"))?;
    Ok(Loaded { path: at, outcome, copied, kept })
}

fn comp_load(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        file: String,
        path: Option<NodePath>,
    }
    let a: A = args(p)?;
    let l = load(e, Actor::Claude, &a.file, a.path, &NodePath::root())?;
    Ok(json!({"path": l.path, "copied": l.copied, "kept": l.kept, "history": history(&l.outcome)}))
}

fn comp_remove(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        place: Option<String>,
    }
    let a: A = args(p)?;
    let to = remove(e, &a.name, a.place.as_deref())?;
    Ok(json!({"moved_to": to.display().to_string()}))
}

fn comp_library(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {}
    let _: A = args(p)?;
    let dirs = json!({
        "project": e.session().dir().join(PROJECT_DIR).display().to_string(),
        "library": e.library().map(|d| d.display().to_string()),
        "penumbra": e.shipped_library().map(|d| d.display().to_string()),
    });
    let list: Vec<Json> = library(e)
        .into_iter()
        .map(|x| json!({"name": x.name, "file": x.file.display().to_string(), "place": x.place}))
        .collect();
    Ok(json!({"dirs": dirs, "components": list}))
}

/// The value of param `name` that node `n` (`id`) declares itself (a shader's header): its own
/// value if set, else the declared default; `None` when it declares no such param (or has not
/// read its file yet).
fn declared(e: &Engine, n: &penumbra_graph::NodeDef, id: NodeId, name: &str) -> Option<penumbra_core::Value> {
    if let Some(v) = n.params.get(name) {
        return Some(v.clone());
    }
    let specs = e.on_cook(move |st| st.cooker.status(id).and_then(|s| s.params)).ok().flatten()?;
    specs.into_iter().find(|s| s.name == name).map(|s| s.default)
}

/// For `node.connect` and `node.disconnect` on a component: its input (or output) called `slot`
/// (a name, or an index in the components' port order) is that port node's only input (output).
pub fn port_node(
    e: &Engine,
    net: &Network,
    path: &NodePath,
    slot: &Json,
    role: PortRole,
) -> Result<Option<NodePath>> {
    let id = require(net, path)?;
    let types = e.types();
    if !net.node(id).and_then(|n| types.desc(&n.kind)).is_some_and(|d| d.container) {
        return Ok(None);
    }
    let (ins, outs) = net.comp_ports(types, id);
    let ports: Vec<NodeId> = if role == PortRole::In { ins } else { outs };
    let names: Vec<String> = ports.iter().filter_map(|p| net.node(*p).map(|n| n.name.to_string())).collect();
    let found = match slot {
        Json::Null => ports.first().copied(),
        Json::Number(n) => n.as_u64().and_then(|i| ports.get(usize::try_from(i).ok()?)).copied(),
        Json::String(s) => {
            ports.iter().copied().find(|p| net.node(*p).is_some_and(|n| n.name.as_str() == s.as_str()))
        }
        _ => None,
    };
    let what = if role == PortRole::In { "input" } else { "output" };
    let id = found.ok_or_else(|| format!("{path} has no {what} {slot}; {what}s: {}", names.join(", ")))?;
    Ok(Some(net.path_of(id).ok_or("port vanished")?))
}

/// The ports of component `id` and its exposed params, for `node.info`.
pub fn info(e: &Engine, net: &Network, id: NodeId) -> Option<Json> {
    let types = e.types();
    let node = net.node(id)?;
    if !types.desc(&node.kind).is_some_and(|d| d.container) {
        return None;
    }
    let (ins, outs) = net.comp_ports(types, id);
    let port = |p: NodeId| {
        let n = net.node(p)?;
        let kind = types.desc(&n.kind)?.inputs.first()?.kind;
        let from = n.inputs.first().copied().flatten().and_then(|s| crate::commands::source_name(e, net, s));
        Some(json!({"name": n.name, "kind": kind, "node": net.path_of(p), "from": from}))
    };
    let mut errors = Vec::new();
    let exposed: Vec<Json> = net
        .exposed_with_errors(types, id, &mut errors)
        .into_iter()
        .map(|x| {
            let target = x.target.as_ref().ok().cloned();
            let resolved = target.as_ref().map(|t| net.resolve_alias(types, t));
            let value = match &resolved {
                Some(Ok(t)) => net.resolve(t.node()).and_then(|nid| {
                    let n = net.node(nid)?;
                    let name = t.param().as_str();
                    n.param(types.desc(&n.kind)?, name).cloned().or_else(|| declared(e, n, nid, name))
                }),
                _ => None,
            };
            let error = match (&x.target, &resolved) {
                (Err(err), _) => Some(err.clone()),
                (_, Some(Err(err))) => Some(err.0.clone()),
                (Ok(t), _) if value.is_none() => Some(format!("{t} does not exist")),
                _ => None,
            };
            json!({"name": x.name, "target": target, "value": value, "error": error})
        })
        .collect();
    Some(json!({
        "inputs": ins.into_iter().filter_map(port).collect::<Vec<_>>(),
        "outputs": outs.into_iter().filter_map(port).collect::<Vec<_>>(),
        "exposed": exposed,
        "expose_errors": errors,
    }))
}
