//! Palette params in nodes: the project's own palettes (the file [`palette::FILE`] in the
//! project directory, reread when it changes) and resolving a param's value against them.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use penumbra_core::palette::{self, Palette};
use penumbra_core::{ParamKind, ParamSpec, Value};
use penumbra_graph::{CookContext, FileWatch};

/// The palettes the file at `path` defines and the notes on lines it skipped. A missing file
/// defines none and is no mistake; one too large or unreadable is a note.
pub fn load(path: &Path) -> (Vec<Palette>, Vec<String>) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), Vec::new()),
        Err(e) => return (Vec::new(), vec![format!("{}: {e}", path.display())]),
    };
    let mut bytes = Vec::new();
    if let Err(e) = file.take(palette::MAX_FILE_BYTES + 1).read_to_end(&mut bytes) {
        return (Vec::new(), vec![format!("{}: {e}", path.display())]);
    }
    if bytes.len() as u64 > palette::MAX_FILE_BYTES {
        return (
            Vec::new(),
            vec![format!("{}: larger than {} KB, not read", path.display(), palette::MAX_FILE_BYTES / 1024)],
        );
    }
    palette::parse_file(&String::from_utf8_lossy(&bytes))
}

/// What a node with palette params keeps: the project's palettes as last read, and a watch on
/// their file so an edit recooks the node.
#[derive(Default)]
pub struct Palettes {
    watch: Option<FileWatch>,
    list: Vec<Palette>,
    notes: Vec<String>,
}

impl Palettes {
    /// The project's own palettes; read the first time and again whenever the file changed. An
    /// unsaved project (no directory) has none.
    pub fn project(&mut self, cx: &CookContext<'_>) -> &[Palette] {
        let Ok(path) = cx.resolve_path(palette::FILE) else { return &[] };
        if !path.is_absolute() {
            return &[];
        }
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        if moved {
            self.watch = Some(cx.watch_file(&path));
        }
        if moved || self.watch.as_ref().is_some_and(FileWatch::take_changed) {
            (self.list, self.notes) = load(&path);
        }
        for n in &self.notes {
            cx.warn(format!("{}: {n}", palette::FILE));
        }
        &self.list
    }

    /// The palette `value` names, lists or picks; `Ok(None)` for an empty value. The project's
    /// file is only read when the built-in set does not answer.
    pub fn resolve(&mut self, cx: &CookContext<'_>, value: &Value) -> Result<Option<Palette>, String> {
        let needs_project = match palette::wanted(value)? {
            palette::Wanted::None | palette::Wanted::Colours(_) => false,
            palette::Wanted::Name(n) => !palette::is_builtin(&n),
            palette::Wanted::Index(_) => true,
        };
        if needs_project {
            let project = self.project(cx).to_vec();
            palette::resolve(value, &project)
        } else {
            palette::resolve(value, &[])
        }
    }

    /// The value of every palette param among `specs` as a pass reads it: the resolved colours
    /// as hex text (a name of the project or a number means nothing to the pass itself). A
    /// value that does not resolve warns and gives the param's default.
    pub fn pass_values(&mut self, cx: &CookContext<'_>, specs: &[ParamSpec]) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for spec in specs.iter().filter(|s| s.kind == ParamKind::Palette) {
            let value = cx.param(&spec.name).cloned().unwrap_or_else(|| spec.default.clone());
            let resolved = match self.resolve(cx, &value) {
                Ok(p) => p,
                Err(e) => {
                    cx.warn(format!("`{}`: {e}; using the default", spec.name));
                    self.resolve(cx, &spec.default).ok().flatten()
                }
            };
            let text = resolved.map(|p| p.hex().join(" ")).unwrap_or_default();
            out.insert(spec.name.clone(), Value::Text(text));
        }
        out
    }
}
