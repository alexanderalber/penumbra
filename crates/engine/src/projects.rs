//! Opening and creating projects while the app runs (`project.open`, `project.new`,
//! `project.demo`, `project.examples`, `project.example`, `project.recent`), the demo project,
//! the shipped examples and the list of recent projects.
//!
//! A project is a directory with a `project.ron`. Opening one switches the running engine over
//! ([`Engine::open_project`]); the window, the GPU and the outputs stay.
//!
//! The recent projects are a small JSON file in the user's data directory
//! ([`default_recent_file`]), newest first. The demo project is left out: it opens on every
//! start without arguments and would always sit on top; the menu offers it separately. So are the
//! examples, which the menu lists too.
//!
//! The examples ship in `examples/` beside the executable (in a checkout: at the repository
//! root, a few levels above it). penumbra saves into the project it has open, so an example is
//! not opened in place: the first time it is copied into `examples` in [`data_dir`], and that
//! copy opens from then on, keeping the user's changes. The shipped folder stays as it is.

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

/// Content of a new demo project.
pub const DEMO: &str = r#"(
    version: 1,
    nodes: {
        "/pulse": (kind: "chan.lfo", params: {"rate": 0.25, "amplitude": 0.4, "offset": 1.0}),
        "/drift": (kind: "chan.noise", params: {"frequency": 0.3, "amplitude": 0.08, "offset": 0.5}),
        "/noise1": (kind: "tex.noise", params: {"color": true, "speed": 0.2}),
        "/rings1": (kind: "tex.shader", params: {"file": "rings.wgsl"}, inputs: {"in0": "noise1"}),
        "/level1": (kind: "tex.level", params: {"contrast": 1.5}, inputs: {"in": "rings1"}, binds: {"brightness": "pulse"}),
        "/title": (kind: "tex.text", params: {"text": "penumbra", "size": 140.0}, binds: {"position.1": "drift"}),
        "/comp1": (kind: "tex.composite", inputs: {"layer1": "level1", "layer2": "title"}),
    },
)"#;

/// The demo's shader, written next to the project when missing.
pub const DEMO_SHADER: &str = "// Rings over the input, moving outwards. Edit and save: the node reloads.
// @param rings float = 6 1..30 -- rings across the image height
// @param tint color = [1, 0.8, 0.6, 1]
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let p = (v.uv - 0.5) * vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let ring = 0.5 + 0.5 * cos(length(p) * params.rings * 6.2831853 - pn_time.seconds * 2.0);
    return vec4f(src.rgb * params.tint.rgb * ring, src.a);
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

/// The copy of example `name` from `source` in `copies`, made unless there is one (with a
/// project file) already.
pub fn copy_example(source: &Path, name: &str, copies: &Path) -> Result<PathBuf, Error> {
    if !list_examples(source).iter().any(|n| n == name) {
        return Err(Error::new(format!("there is no example named `{name}` in {}", source.display())));
    }
    let to = copies.join(name);
    if !to.join(FILE_NAME).is_file() {
        copy_tree(&source.join(name), &to)?;
    }
    Ok(to)
}

/// Copies the files and directories below `from` into `to`; links are skipped.
fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    let fail = |p: &Path, e: std::io::Error| Error::new(format!("{}: {e}", p.display()));
    std::fs::create_dir_all(to).map_err(|e| fail(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| fail(from, e))? {
        let entry = entry.map_err(|e| fail(from, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| fail(&path, e))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&path, &target)?;
        } else if kind.is_file() {
            std::fs::copy(&path, &target).map_err(|e| fail(&path, e))?;
        }
    }
    Ok(())
}

/// Whether the menu lists `dir` anyway: the demo or a copy of an example.
fn listed_elsewhere(dir: &Path) -> bool {
    if demo_dir().is_ok_and(|demo| same_dir(&demo, dir)) {
        return true;
    }
    examples_copy_dir().is_ok_and(|copies| dir.parent().is_some_and(|parent| same_dir(&copies, parent)))
}

/// The list of recent projects, `recent.json` in [`data_dir`].
pub fn default_recent_file() -> Result<PathBuf, Error> {
    Ok(data_dir()?.join("recent.json"))
}

/// Writes the demo's shader into `dir` if it is missing; returns the demo content, the
/// template for a demo project without a project file.
pub fn prepare_demo(dir: &Path, types: &NodeTypes) -> Result<Content, Error> {
    let shader = dir.join("rings.wgsl");
    if !shader.exists() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
        std::fs::write(&shader, DEMO_SHADER).map_err(|e| Error::new(format!("{}: {e}", shader.display())))?;
    }
    penumbra_project::parse(DEMO, types).map_err(|e| e.context("demo"))
}

/// `dir` made absolute (not resolved through links), for comparing and listing.
fn absolute(dir: &Path) -> PathBuf {
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
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
}

/// Every entry of the recent file, also those whose project is gone. A missing, large or
/// broken file is an empty list.
fn read_all(file: &Path) -> Vec<PathBuf> {
    let too_large = std::fs::metadata(file).map(|m| m.len() > RECENT_MAX_BYTES).unwrap_or(true);
    if too_large {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    let mut list = serde_json::from_str::<RecentFile>(&text).map(|r| r.projects).unwrap_or_default();
    list.truncate(RECENT_MAX);
    list
}

/// The recent projects in `file`, newest first, only those that still have a project file.
pub fn read_recent(file: &Path) -> Vec<PathBuf> {
    read_all(file).into_iter().filter(|d| d.join(FILE_NAME).is_file()).collect()
}

/// Puts `dir` on top of the recent projects in `file` (the demo project and the examples are left
/// out).
pub fn remember(file: &Path, dir: &Path) -> Result<(), Error> {
    let dir = absolute(dir);
    if listed_elsewhere(&dir) {
        return Ok(());
    }
    let mut list = read_all(file);
    list.retain(|d| !same_dir(d, &dir) && d.join(FILE_NAME).is_file());
    list.insert(0, dir);
    list.truncate(RECENT_MAX);
    let text = serde_json::to_string_pretty(&RecentFile { projects: list })
        .map_err(|e| Error::new(format!("recent projects: {e}")))?;
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

/// Opens the demo project, made again where it is missing.
pub fn open_demo(engine: &Arc<Engine>) -> Result<Opened, Error> {
    let dir = demo_dir()?;
    let template = prepare_demo(&dir, engine.types())?;
    switch(engine, &dir, &template)
}

/// Opens the shipped example `name`, from its copy in the user's data directory (made on the
/// first open).
pub fn open_example(engine: &Arc<Engine>, name: &str) -> Result<Opened, Error> {
    let source = examples_source().ok_or_else(|| Error::new("no examples folder beside penumbra"))?;
    let dir = copy_example(&source, name, &examples_copy_dir()?)?;
    open(engine, &dir)
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

/// Registers `project.open`, `project.new`, `project.demo`, `project.examples`,
/// `project.example` and `project.recent`.
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
        "project.demo",
        "Open the demo project (made again if missing)",
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
        "Open a shipped example: copied into the user's data folder on the first open, so the \
         shipped one stays as it is; later opens keep the changes; {name}",
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
    fn an_example_is_copied_once_and_then_kept() {
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
        // A second open keeps what the user changed.
        std::fs::write(dir.join(FILE_NAME), "(version: 1, nodes: {})").unwrap();
        copy_example(&source, "a", &copies).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "(version: 1, nodes: {})");
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
