//! Opening and creating projects while the app runs (`project.open`, `project.new`,
//! `project.save_as`, `project.demo`, `project.examples`, `project.example`, `project.recent`),
//! the demo project, the shipped examples and the list of recent projects.
//!
//! A project is a directory with a `project.ron`. Opening one switches the running engine over
//! ([`Engine::open_project`]); the window, the GPU and the outputs stay.
//!
//! The recent projects are a small JSON file in the user's data directory
//! ([`default_recent_file`]), newest first. The demo project is left out: the menu offers it
//! separately. So are the examples, which the menu lists too. The same file names the project
//! open last, whichever it was (the demo and the examples included), which a start without
//! arguments opens again ([`read_last`]).
//!
//! The examples ship in `examples/` beside the executable (in a checkout: at the repository
//! root, a few levels above it). penumbra saves into the project it has open, so an example is
//! not opened in place but copied into `examples` in [`data_dir`]. That copy is made afresh on
//! every open, as is the demo: whatever was changed, saved node state and the editor layout
//! included, goes, so a broken experiment never outlives a reopen. Within a run both save as
//! usual. "Save copy as" (`project.save_as`) turns either into a project of the user's own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use penumbra_core::Error;
use penumbra_graph::NodeTypes;
use penumbra_project::{Content, FILE_NAME, Session};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

/// How many recent projects are kept.
pub const RECENT_MAX: usize = 12;
/// A larger recent file is not read (it is a few hundred bytes when penumbra wrote it).
const RECENT_MAX_BYTES: u64 = 256 * 1024;

/// Content of a new demo project: reaction-diffusion growing around the title. Noise seeds a
/// feedback loop of blur and sharpen, which grows spots and stripes; the title, blurred into a
/// shield, keeps the pattern out of the letters.
pub const DEMO: &str = r#"(
    version: 1,
    nodes: {
        "/noise": (kind: "tex.noise", params: {"width": 640, "height": 360, "keep_size": true, "frequency": 1.27, "octaves": 1, "speed": 0.05}),
        "/paper": (kind: "tex.constant", params: {"width": 640, "height": 360, "keep_size": true, "color": [1.0, 1.0, 1.0, 1.0]}),
        "/title": (kind: "tex.text", params: {"text": "penumbra", "size": 110.0, "style": "Bold", "color": [0.0, 0.0, 0.0, 1.0], "width": 640, "height": 360, "keep_size": true}),
        "/stencil": (kind: "tex.composite", inputs: {"layer1": "paper", "layer2": "title"}),
        "/soften": (kind: "tex.blur", params: {"size": 8.0}, inputs: {"in": "stencil"}),
        "/shield": (kind: "tex.level", params: {"gamma": 0.1}, inputs: {"in": "soften"}),
        "/seed": (kind: "tex.composite", params: {"op": "multiply"}, inputs: {"layer1": "noise", "layer2": "shield"}),
        "/loop": (kind: "tex.feedback", inputs: {"initial": "seed", "previous": "drift"}),
        "/mix": (kind: "tex.cross", params: {"cross": 0.529}, inputs: {"a": "loop", "b": "seed"}),
        "/blur": (kind: "tex.blur", params: {"size": 6.0}, inputs: {"in": "mix"}),
        "/sharpen": (kind: "tex.shader", params: {"file": "sharpen.wgsl", "amount": 0.99}, inputs: {"in0": "blur"}),
        "/keep": (kind: "tex.composite", params: {"op": "multiply"}, inputs: {"layer1": "sharpen", "layer2": "shield"}),
        "/drift": (kind: "tex.transform", inputs: {"in": "keep"}, binds: {"rotate": "wobble"}),
        "/wobble": (kind: "chan.noise", params: {"frequency": 0.01, "amplitude": 0.04}),
        "/full": (kind: "tex.fit", params: {"fit": "stretch"}, inputs: {"in": "drift"}),
        "/palette0": (kind: "tex.ramp", params: {"width": 256, "height": 1, "keys": "0 0.8308 0.624 0.1878\n0.1 0.8308 0.624 0.1878\n0.3 0.6867 0.1046 0.0561\n0.5 0.521 0.0222 0.0545\n0.7 0.0887 0.0176 0.0382\n0.9 0.0865 0.1845 0.1946\n1 0.0865 0.1845 0.1946"}),
        "/palette1": (kind: "tex.ramp", params: {"width": 256, "height": 1, "keys": "0 0.0529 0.0482 0.0685\n0.1 0.0529 0.0482 0.0685\n0.3 0.1714 0.1221 0.1559\n0.5 0.4452 0.2159 0.227\n0.7 0.8632 0.4508 0.3372\n0.9 0.9216 0.7682 0.5089\n1 0.9216 0.7682 0.5089"}),
        "/palette2": (kind: "tex.ramp", params: {"width": 256, "height": 1, "keys": "0 0.0343 0.0395 0.0395\n0.1 0.0343 0.0395 0.0395\n0.3 0.0 0.1144 0.147\n0.5 0.0 0.2582 0.3372\n0.7 0.0 0.4508 0.5972\n0.9 0.0 0.7305 0.9647\n1 0.0 0.7305 0.9647"}),
        "/palette3": (kind: "tex.ramp", params: {"width": 256, "height": 1, "keys": "0 0.0296 0.0 0.0296\n0.1 0.0296 0.0 0.0296\n0.3 0.0648 0.0 0.0648\n0.5 0.117 0.0091 0.0648\n0.7 0.521 0.0648 0.0648\n0.9 0.8632 0.1683 0.0529\n1 0.8632 0.1683 0.0529"}),
        "/palettes": (kind: "tex.switch", params: {"blend": true}, inputs: {"in0": "palette0", "in1": "palette1", "in2": "palette2", "in3": "palette3"}),
        "/show": (kind: "tex.lookup", inputs: {"in": "full", "lut": "palettes"}),
    },
)"#;

/// The demo's shader, written next to the project when missing.
pub const DEMO_SHADER: &str =
    "// A strong 5 x 5 sharpen, clamped to 0..1. With a blur before it in a feedback loop, the pair
// grows reaction-diffusion patterns: spots and stripes. Edit and save: the node reloads.
// The kernel is binomial with its centre replaced, divided by its sum. The pattern only forms
// near a threshold of blur against sharpen: try amount 1, or 0.97.
// @param amount float = 1 0..1.3 -- 1: centre -225 (sum -5), 0: centre -840
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    var b = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
    let centre = -840.0 + params.amount * 615.0;
    var sum = vec4f(0.0);
    var total = 0.0;
    for (var j = 0; j < 5; j++) {
        for (var i = 0; i < 5; i++) {
            var w = b[i] * b[j];
            if (i == 2 && j == 2) {
                w = centre;
            }
            let uv = v.uv + vec2f(f32(i - 2), f32(j - 2)) * pn.texel;
            sum += w * textureSampleLevel(in0, pn_clamp, uv, 0.0);
            total += w;
        }
    }
    let s = select(sum, sum / total, abs(total) > 1e-3);
    return clamp(s, vec4f(0.0), vec4f(1.0));
}
";

/// penumbra's directory in the user's data directory: `%LOCALAPPDATA%\penumbra` on Windows,
/// `$XDG_DATA_HOME/penumbra` (or `~/.local/share/penumbra`) elsewhere. Outside any repository,
/// so autosave never touches tracked files.
pub fn data_dir() -> Result<PathBuf, Error> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("LOCALAPPDATA").ok_or_else(|| Error::new("LOCALAPPDATA is not set"))?
    } else {
        match var("XDG_DATA_HOME") {
            Some(d) => d,
            None => var("HOME")
                .ok_or_else(|| Error::new("neither XDG_DATA_HOME nor HOME is set"))?
                .join(".local/share"),
        }
    };
    Ok(base.join("penumbra"))
}

/// The demo project's directory, `demo` in [`data_dir`].
pub fn demo_dir() -> Result<PathBuf, Error> {
    Ok(data_dir()?.join("demo"))
}

/// Where opened examples are copied to, `examples` in [`data_dir`].
pub fn examples_copy_dir() -> Result<PathBuf, Error> {
    Ok(data_dir()?.join("examples"))
}

/// How many directories above the executable's own the shipped examples are looked for: the
/// release zip has them beside it, a checkout three levels up (`target/debug/deps` for tests).
const EXAMPLES_SEARCH_DEPTH: usize = 4;

/// The shipped `examples` directory: beside the executable or a few levels above it, the
/// first one holding examples (cargo keeps an `examples` directory of its own beside the
/// executable in a checkout).
pub fn examples_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(EXAMPLES_SEARCH_DEPTH + 1)
        .map(|d| d.join("examples"))
        .find(|d| !list_examples(d).is_empty())
}

/// The examples in `source`: its subdirectories holding a project file, by name.
pub fn list_examples(source: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(source) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.path().join(FILE_NAME).is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Where the copy of example `name` from `source` goes in `copies`; an error unless `source`
/// has that example (so `name` cannot reach outside `copies`).
fn example_copy(source: &Path, name: &str, copies: &Path) -> Result<PathBuf, Error> {
    if !list_examples(source).iter().any(|n| n == name) {
        return Err(Error::new(format!("there is no example named `{name}` in {}", source.display())));
    }
    Ok(copies.join(name))
}

/// Makes the copy of example `name` from `source` in `copies` afresh: whatever the folder held
/// (changes, saved state, layout) goes first.
pub fn copy_example(source: &Path, name: &str, copies: &Path) -> Result<PathBuf, Error> {
    let to = example_copy(source, name, copies)?;
    clear_dir(&to)?;
    copy_tree(&source.join(name), &to, &[])?;
    Ok(to)
}

/// Copies the files and directories below `from` into `to`, except the entries of `from`
/// itself named in `skip`; links are skipped.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result<(), Error> {
    let fail = |p: &Path, e: std::io::Error| Error::new(format!("{}: {e}", p.display()));
    std::fs::create_dir_all(to).map_err(|e| fail(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| fail(from, e))? {
        let entry = entry.map_err(|e| fail(from, e))?;
        if entry.file_name().to_str().is_some_and(|n| skip.contains(&n)) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| fail(&path, e))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&path, &target, &[])?;
        } else if kind.is_file() {
            std::fs::copy(&path, &target).map_err(|e| fail(&path, e))?;
        }
    }
    Ok(())
}

/// Deletes everything inside `dir` (missing is fine). The directory itself stays, since a
/// file watcher may still hold it open.
fn clear_dir(dir: &Path) -> Result<(), Error> {
    let fail = |p: &Path, e: std::io::Error| Error::new(format!("cannot delete {}: {e}", p.display()));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(fail(dir, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| fail(dir, e))?;
        let path = entry.path();
        let is_dir = entry.file_type().map_err(|e| fail(&path, e))?.is_dir();
        if is_dir {
            std::fs::remove_dir_all(&path).map_err(|e| fail(&path, e))?;
        } else {
            std::fs::remove_file(&path).map_err(|e| fail(&path, e))?;
        }
    }
    Ok(())
}

/// Whether `dir` is the demo project.
pub fn is_demo(dir: &Path) -> bool {
    demo_dir().is_ok_and(|demo| same_dir(&demo, dir))
}

/// The example whose copy `dir` is, by name.
pub fn example_of(dir: &Path) -> Option<String> {
    let copies = examples_copy_dir().ok()?;
    let dir = absolute(dir);
    if !same_dir(&copies, dir.parent()?) {
        return None;
    }
    dir.file_name()?.to_str().map(str::to_string)
}

/// Whether `dir` is made afresh on every open (the demo or a copy of an example), so changes
/// there do not outlive a reopen. The menu lists these on their own, not among the recent ones.
pub fn starts_fresh(dir: &Path) -> bool {
    is_demo(dir) || example_of(dir).is_some()
}

/// The list of recent projects, `recent.json` in [`data_dir`].
pub fn default_recent_file() -> Result<PathBuf, Error> {
    Ok(data_dir()?.join("recent.json"))
}

/// Writes the demo's shader into `dir` if it is missing; returns the demo content, the
/// template for a demo project without a project file.
pub fn prepare_demo(dir: &Path, types: &NodeTypes) -> Result<Content, Error> {
    let shader = dir.join("sharpen.wgsl");
    if !shader.exists() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
        std::fs::write(&shader, DEMO_SHADER).map_err(|e| Error::new(format!("{}: {e}", shader.display())))?;
    }
    penumbra_project::parse(DEMO, types).map_err(|e| e.context("demo"))
}

/// Makes the demo project in `dir` afresh: whatever the folder held goes, the shader is written
/// again; returns the content its project file is to be made from.
pub fn fresh_demo(dir: &Path, types: &NodeTypes) -> Result<Content, Error> {
    clear_dir(dir)?;
    prepare_demo(dir, types)
}

/// Makes `dir` afresh if it is the demo or an example's copy, for a start that opens it again;
/// returns the template for a missing project file. Other projects are left alone.
pub fn refresh(dir: &Path, types: &NodeTypes) -> Result<Content, Error> {
    if is_demo(dir) {
        return fresh_demo(dir, types);
    }
    if let Some(name) = example_of(dir) {
        let source = examples_source().ok_or_else(|| Error::new("no examples folder beside penumbra"))?;
        copy_example(&source, &name, &examples_copy_dir()?)?;
    }
    Ok(Content::default())
}

/// `dir` made absolute (not resolved through links), for comparing and listing.
fn absolute(dir: &Path) -> PathBuf {
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// `dir` as compared: absolute, lower case on Windows.
fn comparable(dir: &Path) -> PathBuf {
    let dir = absolute(dir);
    if cfg!(windows) { PathBuf::from(dir.to_string_lossy().to_lowercase()) } else { dir }
}

/// Whether `inner` is `outer` or lies below it.
fn within(inner: &Path, outer: &Path) -> bool {
    comparable(inner).starts_with(comparable(outer))
}

/// Whether two project directories are the same (case-insensitive on Windows).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let (a, b) = (absolute(a), absolute(b));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[derive(Default, serde::Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    projects: Vec<PathBuf>,
    /// The project open last, also when it is the demo or an example.
    #[serde(default)]
    last: Option<PathBuf>,
}

/// The recent file with every entry, also those whose project is gone. A missing, large or
/// broken file is an empty one.
fn read_file(file: &Path) -> RecentFile {
    let too_large = std::fs::metadata(file).map(|m| m.len() > RECENT_MAX_BYTES).unwrap_or(true);
    if too_large {
        return RecentFile::default();
    }
    let Ok(text) = std::fs::read_to_string(file) else { return RecentFile::default() };
    let mut recent = serde_json::from_str::<RecentFile>(&text).unwrap_or_default();
    recent.projects.truncate(RECENT_MAX);
    recent
}

/// The recent projects in `file`, newest first, only those that still have a project file.
pub fn read_recent(file: &Path) -> Vec<PathBuf> {
    read_file(file).projects.into_iter().filter(|d| d.join(FILE_NAME).is_file()).collect()
}

/// The project open last according to `file`, if it still has a project file.
pub fn read_last(file: &Path) -> Option<PathBuf> {
    read_file(file).last.filter(|d| d.join(FILE_NAME).is_file())
}

/// Notes `dir` as the project open last and puts it on top of the recent projects in `file` (the
/// demo project and the examples are left out of that list).
pub fn remember(file: &Path, dir: &Path) -> Result<(), Error> {
    let dir = absolute(dir);
    let mut recent = read_file(file);
    recent.last = Some(dir.clone());
    if !starts_fresh(&dir) {
        recent.projects.retain(|d| !same_dir(d, &dir) && d.join(FILE_NAME).is_file());
        recent.projects.insert(0, dir);
        recent.projects.truncate(RECENT_MAX);
    }
    let text =
        serde_json::to_string_pretty(&recent).map_err(|e| Error::new(format!("recent projects: {e}")))?;
    let fail = |e: std::io::Error| Error::new(format!("cannot write {}: {e}", file.display()));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, file).map_err(fail)
}

/// What opening a project did, for the reply and the status bar.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Opened {
    pub dir: String,
    /// `false` when the project was open already.
    pub switched: bool,
}

/// Switches to the project in `dir` (made from `template` when it has no project file).
fn switch(engine: &Arc<Engine>, dir: &Path, template: &Content) -> Result<Opened, Error> {
    let shown = absolute(dir).display().to_string();
    if same_dir(engine.session().dir(), dir) {
        return Ok(Opened { dir: shown, switched: false });
    }
    let types = engine.types().clone();
    let session = Arc::new(Session::open(&absolute(dir), types, template)?);
    engine.open_project(session)?;
    log::info!("project: {shown}");
    Ok(Opened { dir: shown, switched: true })
}

/// Opens the existing project in `dir`.
pub fn open(engine: &Arc<Engine>, dir: &Path) -> Result<Opened, Error> {
    if !dir.join(FILE_NAME).is_file() {
        return Err(Error::new(format!(
            "{} holds no {FILE_NAME}; project.new makes a project there",
            dir.display()
        )));
    }
    switch(engine, dir, &Content::default())
}

/// Makes an empty project in `dir` (created if missing; other files there stay) and opens it.
pub fn create(engine: &Arc<Engine>, dir: &Path) -> Result<Opened, Error> {
    if dir.join(FILE_NAME).exists() {
        return Err(Error::new(format!("{} is a project already; project.open opens it", dir.display())));
    }
    if dir.is_file() {
        return Err(Error::new(format!("{} is a file, not a directory", dir.display())));
    }
    switch(engine, dir, &Content::default())
}

/// Switches to `dir` after `fill` made it afresh (and returned the template for its project
/// file). That happens once the open project is closed, so the open project may be `dir`
/// itself; its closing state snapshot is skipped then, since the folder is replaced anyway.
pub(crate) fn open_fresh<F>(engine: &Arc<Engine>, dir: &Path, fill: F) -> Result<Opened, Error>
where
    F: FnOnce(&Path) -> Result<Content, Error>,
{
    let target = absolute(dir);
    let shown = target.display().to_string();
    let reopen = same_dir(engine.session().dir(), &target);
    let types = engine.types().clone();
    engine.replace_project(!reopen, move || {
        let template = fill(&target)?;
        Ok(Arc::new(Session::open(&target, types, &template)?))
    })?;
    log::info!("project (fresh): {shown}");
    Ok(Opened { dir: shown, switched: true })
}

/// Opens the demo project, made afresh (also when it is open: then it starts over).
pub fn open_demo(engine: &Arc<Engine>) -> Result<Opened, Error> {
    let dir = demo_dir()?;
    let types = engine.types().clone();
    open_fresh(engine, &dir, move |d| fresh_demo(d, &types))
}

/// Opens the shipped example `name` from a fresh copy in the user's data directory (also when
/// it is open: then it starts over).
pub fn open_example(engine: &Arc<Engine>, name: &str) -> Result<Opened, Error> {
    let source = examples_source().ok_or_else(|| Error::new("no examples folder beside penumbra"))?;
    let copies = examples_copy_dir()?;
    let dir = example_copy(&source, name, &copies)?;
    open_fresh(engine, &dir, |_| copy_example(&source, name, &copies).map(|_| Content::default()))
}

/// Writes the open project into `dir` (new, or an empty folder) and switches to the copy, a
/// project like any other. Everything in the project folder goes along (shaders, scripts,
/// media, the layout) except the state folder, of which only the latest snapshot is taken, so
/// feedback loops and locked nodes go on where they were.
pub fn save_copy(engine: &Arc<Engine>, dir: &Path) -> Result<Opened, Error> {
    let target = absolute(dir);
    let current = absolute(engine.session().dir());
    if target.is_file() {
        return Err(Error::new(format!("{} is a file, not a folder", target.display())));
    }
    let occupied = std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(false);
    if occupied {
        return Err(Error::new(format!("{} is not empty; pick a new or empty folder", target.display())));
    }
    if within(&target, &current) {
        return Err(Error::new("the copy cannot go inside the project itself"));
    }
    let shown = target.display().to_string();
    let types = engine.types().clone();
    engine.replace_project(true, move || {
        // The old project is closed by now: its changes and its state are on disk.
        copy_tree(&current, &target, &[crate::state::DIR])?;
        let latest = crate::state::dir_of(&current).join(crate::state::LATEST);
        if latest.is_file() {
            let to = crate::state::dir_of(&target);
            std::fs::create_dir_all(&to).map_err(|e| Error::new(format!("{}: {e}", to.display())))?;
            std::fs::copy(&latest, to.join(crate::state::LATEST))
                .map_err(|e| Error::new(format!("{}: {e}", latest.display())))?;
        }
        Ok(Arc::new(Session::open(&target, types, &Content::default())?))
    })?;
    log::info!("project (copy): {shown}");
    Ok(Opened { dir: shown, switched: true })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArg {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirArg {
    dir: PathBuf,
}

fn reply(r: Result<Opened, Error>) -> Result<Json, CommandError> {
    let opened = r?;
    Ok(json!({"dir": opened.dir, "switched": opened.switched}))
}

/// Registers `project.open`, `project.new`, `project.save_as`, `project.demo`,
/// `project.examples`, `project.example` and `project.recent`.
pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.open",
        "Close this project (state saved, changes written) and open the one in dir; {dir}",
        move |p| reply(open(&e, &args::<DirArg>(p)?.dir)),
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.new",
        "Make an empty project in dir (created if missing, must hold no project.ron) and open it; {dir}",
        move |p| reply(create(&e, &args::<DirArg>(p)?.dir)),
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.save_as",
        "Copy the open project (its files and the latest state snapshot) into dir (new or empty) and open the copy, a project of its own; {dir}",
        move |p| reply(save_copy(&e, &args::<DirArg>(p)?.dir)),
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.demo",
        "Open the demo project, made afresh: changes, state and layout from before are gone",
        move |p| {
            args::<crate::commands::Empty>(p)?;
            reply(open_demo(&e))
        },
    ));
    registry.register(FnCommand::new(
        "project.examples",
        "The examples shipped with penumbra, by name, and where they lie",
        move |p| {
            args::<crate::commands::Empty>(p)?;
            let source = examples_source();
            let names = source.as_deref().map(list_examples).unwrap_or_default();
            Ok(json!({"examples": names, "dir": source.map(|d| d.display().to_string())}))
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.example",
        "Open a shipped example from a fresh copy in the user's data folder (the shipped one stays \
         as it is); changes last until it is opened again, project.save_as keeps them; {name}",
        move |p| reply(open_example(&e, &args::<NameArg>(p)?.name)),
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "project.recent",
        "Recently opened projects, newest first (the demo is not listed), and the open one",
        move |p| {
            args::<crate::commands::Empty>(p)?;
            let recent: Vec<String> = e.recent().iter().map(|d| d.display().to_string()).collect();
            Ok(json!({"recent": recent, "open": absolute(e.session().dir()).display().to_string()}))
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("penumbra-projects-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recent_list_keeps_the_newest_first_and_drops_what_is_gone() {
        let root = temp("recent");
        let file = root.join("recent.json");
        let dirs: Vec<PathBuf> = (0..3).map(|i| root.join(format!("p{i}"))).collect();
        for d in &dirs {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(FILE_NAME), "(version: 1)").unwrap();
            remember(&file, d).unwrap();
        }
        remember(&file, &dirs[0]).unwrap();
        let list = read_recent(&file);
        assert_eq!(list.len(), 3);
        assert!(same_dir(&list[0], &dirs[0]));
        assert!(same_dir(&list[1], &dirs[2]));
        std::fs::remove_file(dirs[2].join(FILE_NAME)).unwrap();
        assert_eq!(read_recent(&file).len(), 2);
        for i in 0..RECENT_MAX + 3 {
            let d = root.join(format!("q{i}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(FILE_NAME), "(version: 1)").unwrap();
            remember(&file, &d).unwrap();
        }
        assert_eq!(read_recent(&file).len(), RECENT_MAX);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_last_project_is_kept_also_when_the_list_leaves_it_out() {
        let root = temp("last");
        let file = root.join("recent.json");
        assert_eq!(read_last(&file), None);
        let p = root.join("p");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(FILE_NAME), "(version: 1)").unwrap();
        remember(&file, &p).unwrap();
        assert!(read_last(&file).is_some_and(|d| same_dir(&d, &p)));
        // The demo is the last project but stays off the list.
        let demo = demo_dir().unwrap();
        remember(&file, &demo).unwrap();
        let list = read_recent(&file);
        assert_eq!(list.len(), 1);
        assert!(same_dir(&list[0], &p));
        let last = read_file(&file).last.unwrap();
        assert!(same_dir(&last, &demo));
        // A last project without a project file any more is none.
        remember(&file, &p).unwrap();
        std::fs::remove_file(p.join(FILE_NAME)).unwrap();
        assert_eq!(read_last(&file), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_example_is_copied_afresh_every_time() {
        let root = temp("examples");
        let source = root.join("examples");
        for name in ["b", "a"] {
            std::fs::create_dir_all(source.join(name).join("sub")).unwrap();
            std::fs::write(source.join(name).join(FILE_NAME), "(version: 1)").unwrap();
            std::fs::write(source.join(name).join("sub").join("x.wgsl"), "// x").unwrap();
        }
        std::fs::create_dir_all(source.join("not-a-project")).unwrap();
        std::fs::write(source.join("README.md"), "# Examples").unwrap();
        assert_eq!(list_examples(&source), ["a", "b"]);

        let copies = root.join("copies");
        let dir = copy_example(&source, "a", &copies).unwrap();
        assert!(dir.join("sub").join("x.wgsl").is_file());
        // A second open drops what the user changed, saved state and layout included.
        std::fs::write(dir.join(FILE_NAME), "(version: 1, nodes: {})").unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::write(dir.join("state").join("latest.pnstate"), "x").unwrap();
        std::fs::write(dir.join("layout.ron"), "()").unwrap();
        copy_example(&source, "a", &copies).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "(version: 1)");
        assert!(!dir.join("state").exists());
        assert!(!dir.join("layout.ron").exists());
        assert!(dir.join("sub").join("x.wgsl").is_file());
        for bad in ["c", "not-a-project", "..", "../examples/a", ""] {
            assert!(copy_example(&source, bad, &copies).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_broken_recent_file_is_an_empty_list() {
        let root = temp("broken");
        let file = root.join("recent.json");
        assert!(read_recent(&file).is_empty());
        std::fs::write(&file, "{\"projects\": 7").unwrap();
        assert!(read_recent(&file).is_empty());
        std::fs::write(&file, [0xffu8, 0xfe, 0x00]).unwrap();
        assert!(read_recent(&file).is_empty());
        // Writing over it starts a new list.
        let d = root.join("p");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(FILE_NAME), "(version: 1)").unwrap();
        remember(&file, &d).unwrap();
        assert_eq!(read_recent(&file).len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
