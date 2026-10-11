//! An open project: the document, who may edit it, and keeping it in step with `project.ron`.
//!
//! - **Autosave:** about [`AUTOSAVE_DELAY`] after the document first differs from the file, the
//!   file is rewritten (in canonical form, atomically through a temporary file). A continuous
//!   drag is written once per delay, not only at its end.
//! - **External edits:** the file is read on every [`Session::poll`] (a few times per second)
//!   and compared with the text last written or read; comparing text rather than timestamps
//!   cannot miss an edit that lands in the same timestamp tick. A change waits for the
//!   [`Gate`], is diffed against the content it was based on and merged into the running
//!   document as one history entry with source "project file".
//! - **A file that does not merge** (syntax error, unknown kind, a conflict with the user's
//!   unsaved edits) is reported in [`SessionStatus::file_error`], and autosave pauses so the
//!   broken file, which is probably someone's work in progress, is not overwritten. The next
//!   change to the file is tried again; `project.save` overwrites it on purpose.
//!
//! File IO happens outside the state lock, so a slow disk never stalls an edit from the UI.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use penumbra_core::{Error, GroupId};
use penumbra_graph::{Document, EditOutcome, Network, NodeTypes, Op, Params, Source};
use serde::Serialize;

use crate::content::{Content, diff};
use crate::file::{FILE_NAME, MAX_FILE_BYTES, parse, print};
use crate::gate::{Gate, GateStatus};

/// Delay between the first unsaved change and writing the file.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// Who asks for an edit; decides which rule of the [`Gate`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    /// The user, through the UI.
    User,
    /// Claude (or any other client), through the control channel.
    Claude,
    /// What OSC learn binds. Passes the gate like the UI (refused while Claude holds the claim,
    /// and counts as the user editing), recorded with source [`Source::Osc`]. Param values from
    /// OSC messages go through [`Session::edit_live`] instead and are not recorded.
    Osc,
}

/// `project.info`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionStatus {
    pub dir: String,
    pub nodes: usize,
    /// The document differs from the file (autosave is due or paused).
    pub unsaved: bool,
    /// An external edit of the file waits for the user to stop editing.
    pub merge_waiting: bool,
    pub file_error: Option<String>,
    pub write_error: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub edit: GateStatus,
}

struct State {
    gate: Gate,
    /// Content of the file as last written or merged: the base of the next merge.
    base: Content,
    /// Text of the file as last written or read; `None` when there is no file.
    file_text: Option<String>,
    /// The snapshot the file holds.
    written: Arc<Network>,
    /// File text that changed and waits for the gate.
    waiting: Option<String>,
    file_error: Option<String>,
    write_error: Option<String>,
    dirty_since: Option<Instant>,
}

pub struct Session {
    dir: PathBuf,
    file: PathBuf,
    doc: Document,
    /// Held for a whole poll or save, so file reads and writes never interleave.
    io: Mutex<()>,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The network with `content`, built from scratch.
fn build(content: &Content, types: &NodeTypes) -> Result<Network, Error> {
    Ok(Network::new().apply_with(types, &diff(&Content::default(), content), Params::Keep)?.network)
}

/// The file's text, `None` if it does not exist.
fn read_text(path: &Path) -> Result<Option<String>, Error> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::new(format!("cannot read {}: {e}", path.display()))),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::new(format!("{} is larger than {} MiB", path.display(), MAX_FILE_BYTES >> 20)));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| Error::new(format!("{} is not UTF-8 text", path.display())))
}

/// Writes through a temporary file and a rename, so a reader never sees half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), Error> {
    let tmp = path.with_extension("ron.tmp");
    let fail = |e: std::io::Error| Error::new(format!("cannot write {}: {e}", path.display()));
    let mut f = std::fs::File::create(&tmp).map_err(fail)?;
    f.write_all(text.as_bytes()).map_err(fail)?;
    f.sync_all().map_err(fail)?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(fail)
}

fn same(a: &Arc<Network>, b: &Arc<Network>) -> bool {
    Arc::ptr_eq(a, b) || a == b
}

impl Session {
    /// Opens the project in `dir`, creating the directory and, if there is no project file yet,
    /// writing `template` as its content. A project file that cannot be read or does not load
    /// opens as an empty document with the reason in [`SessionStatus::file_error`]; the file is
    /// left alone and loaded as soon as it is fixed.
    pub fn open(dir: &Path, types: Arc<NodeTypes>, template: &Content) -> Result<Session, Error> {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::new(format!("cannot create the project directory {}: {e}", dir.display())))?;
        let file = dir.join(FILE_NAME);
        let loaded = read_text(&file).and_then(|text| match text {
            None => Ok(None),
            Some(text) => {
                let content = parse(&text, &types)?;
                let net = build(&content, &types)?;
                Ok(Some((text, content, net)))
            }
        });
        let (net, base, file_text, file_error) = match loaded {
            Ok(Some((text, content, net))) => (net, content, Some(text), None),
            Ok(None) => {
                let net = build(template, &types).map_err(|e| e.context("the project template"))?;
                let content = Content::from_network(&net)?;
                let text = print(&content, &types)?;
                write_atomic(&file, &text)?;
                (net, content, Some(text), None)
            }
            Err(e) => {
                log::error!("{e}");
                // The text stays unknown, so the next poll reads the file again and keeps
                // reporting the error until the file loads.
                (Network::new(), Content::default(), None, Some(e.0))
            }
        };
        let doc = Document::new(types, net);
        let written = doc.snapshot();
        Ok(Session {
            dir: dir.to_path_buf(),
            file,
            doc,
            io: Mutex::new(()),
            state: Mutex::new(State {
                gate: Gate::new(),
                base,
                file_text,
                written,
                waiting: None,
                file_error,
                write_error: None,
                dirty_since: None,
            }),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn types(&self) -> &Arc<NodeTypes> {
        self.doc.types()
    }

    /// Applies `ops` as one batch, if the gate lets `actor` edit now. `group` matters for the
    /// user and OSC learn only (one per parameter drag; `None` for a one-off edit); Claude's edits are grouped by its claim.
    pub fn edit(
        &self,
        actor: Actor,
        ops: &[Op],
        group: Option<GroupId>,
        label: &str,
        now: Instant,
    ) -> Result<EditOutcome, Error> {
        let mut state = lock(&self.state);
        let (source, group) = match actor {
            Actor::User => {
                state.gate.user_edit(now)?;
                (Source::Ui, group)
            }
            Actor::Osc => {
                state.gate.user_edit(now)?;
                (Source::Osc, group)
            }
            Actor::Claude => (Source::ControlChannel, state.gate.claude_edit(now)?),
        };
        let group = group.unwrap_or_else(|| self.doc.new_group());
        self.doc.edit(ops, source, group, label)
    }

    /// Sets param values outside the history ([`Document::edit_live`]), if the gate lets the
    /// user edit now: OSC param edits, live control as in TouchDesigner.
    pub fn edit_live(&self, ops: &[Op], now: Instant) -> Result<Arc<Network>, Error> {
        let mut state = lock(&self.state);
        state.gate.user_edit(now)?;
        self.doc.edit_live(ops)
    }

    /// Undo one history entry; returns its label, `None` when there is nothing to undo.
    pub fn undo(&self, actor: Actor, now: Instant) -> Result<Option<String>, Error> {
        self.history_step(actor, now, true)
    }

    pub fn redo(&self, actor: Actor, now: Instant) -> Result<Option<String>, Error> {
        self.history_step(actor, now, false)
    }

    fn history_step(&self, actor: Actor, now: Instant, back: bool) -> Result<Option<String>, Error> {
        let mut state = lock(&self.state);
        match actor {
            Actor::User | Actor::Osc => state.gate.user_edit(now)?,
            Actor::Claude => {
                state.gate.claude_edit(now)?;
            }
        }
        Ok(if back { self.doc.undo() } else { self.doc.redo() })
    }

    /// `edit.begin`.
    pub fn begin(&self, label: &str, now: Instant) -> Result<GroupId, Error> {
        let group = self.doc.new_group();
        lock(&self.state).gate.begin(now, label, group)
    }

    /// `edit.end`; whether Claude held the claim.
    pub fn end(&self, now: Instant) -> bool {
        lock(&self.state).gate.end(now)
    }

    /// The UI's "take over".
    pub fn take_over(&self) {
        lock(&self.state).gate.take_over();
    }

    pub fn status(&self, now: Instant) -> SessionStatus {
        let mut state = lock(&self.state);
        let current = self.doc.snapshot();
        let (can_undo, can_redo) = self.doc.with_history(|h| (h.can_undo(), h.can_redo()));
        SessionStatus {
            dir: self.dir.display().to_string(),
            nodes: current.len(),
            unsaved: state.file_text.is_none() || !same(&current, &state.written),
            merge_waiting: state.waiting.is_some(),
            file_error: state.file_error.clone(),
            write_error: state.write_error.clone(),
            can_undo,
            can_redo,
            edit: state.gate.status(now),
        }
    }

    /// Called a few times per second from the project thread: picks up external edits of the
    /// file, merges them when the gate allows, and autosaves.
    pub fn poll(&self, now: Instant) {
        let _io = lock(&self.io);
        self.look_at_file();
        self.merge_waiting(now);
        self.autosave(now);
    }

    /// Like [`Session::poll`], but writes unsaved changes at once instead of after the delay.
    /// For closing the app. An edit of the file that has not merged yet (waiting for the gate,
    /// or failed) is left alone, as autosave would.
    pub fn flush(&self, now: Instant) {
        let _io = lock(&self.io);
        self.look_at_file();
        self.merge_waiting(now);
        let paused = {
            let state = lock(&self.state);
            state.waiting.is_some() || state.file_error.is_some()
        };
        if !paused && let Err(e) = self.write(false) {
            log::error!("saving on close: {e}");
        }
    }

    /// Writes the document now, also over a file that failed to merge or an edit still waiting
    /// for the gate (those are discarded). Returns whether the file changed.
    pub fn save(&self) -> Result<bool, Error> {
        let _io = lock(&self.io);
        {
            let mut state = lock(&self.state);
            state.waiting = None;
            state.file_error = None;
        }
        self.write(true)
    }

    fn look_at_file(&self) {
        let read = read_text(&self.file);
        let mut state = lock(&self.state);
        match read {
            Ok(text) if text == state.file_text => {}
            Ok(None) => {
                log::warn!("{} disappeared; writing it again", self.file.display());
                state.file_text = None;
                state.waiting = None;
            }
            Ok(Some(text)) => {
                state.file_text = Some(text.clone());
                state.waiting = Some(text);
            }
            Err(e) => {
                if state.file_error.as_deref() != Some(e.0.as_str()) {
                    log::warn!("{e}");
                }
                state.file_error = Some(e.0);
            }
        }
    }

    fn merge_waiting(&self, now: Instant) {
        let mut state = lock(&self.state);
        if state.waiting.is_none() || !state.gate.file_merge_allowed(now) {
            return;
        }
        let Some(text) = state.waiting.take() else { return };
        let types = self.doc.types();
        let merged = parse(&text, types).and_then(|theirs| {
            let ops = diff(&state.base, &theirs);
            if !ops.is_empty() {
                self.doc.edit(
                    &ops,
                    Source::ProjectFile,
                    self.doc.new_group(),
                    &format!("edit of {FILE_NAME}"),
                )?;
            }
            Ok(theirs)
        });
        match merged {
            Ok(theirs) => {
                if state.file_error.take().is_some() {
                    log::info!("{} loads again", self.file.display());
                }
                state.base = theirs;
            }
            Err(e) => {
                log::warn!("{FILE_NAME} not merged: {e}");
                state.file_error = Some(e.0);
            }
        }
    }

    fn autosave(&self, now: Instant) {
        {
            let mut state = lock(&self.state);
            if state.waiting.is_some() || state.file_error.is_some() {
                return;
            }
            let dirty = state.file_text.is_none() || !same(&self.doc.snapshot(), &state.written);
            if !dirty {
                state.dirty_since = None;
                return;
            }
            let since = *state.dirty_since.get_or_insert(now);
            if now.saturating_duration_since(since) < AUTOSAVE_DELAY {
                return;
            }
        }
        if let Err(e) = self.write(false) {
            let mut state = lock(&self.state);
            if state.write_error.as_deref() != Some(e.0.as_str()) {
                log::error!("autosave: {e}");
            }
            state.write_error = Some(e.0);
        }
    }

    /// Writes the current snapshot unless the file already holds it. Unless `force`d, the file
    /// is checked first: if it changed since it was last read, nothing is written and the next
    /// poll merges the change instead.
    fn write(&self, force: bool) -> Result<bool, Error> {
        let current = self.doc.snapshot();
        let content = Content::from_network(&current)?;
        let text = print(&content, self.doc.types())?;
        let on_disk = if force { None } else { Some(read_text(&self.file)) };
        let mut state = lock(&self.state);
        if let Some(on_disk) = on_disk
            && on_disk.as_ref().ok() != Some(&state.file_text)
        {
            return Ok(false);
        }
        let changed = state.file_text.as_ref() != Some(&text);
        if changed {
            drop(state);
            write_atomic(&self.file, &text)?;
            state = lock(&self.state);
            state.file_text = Some(text);
        }
        state.written = current;
        state.base = content;
        state.dirty_since = None;
        state.write_error = None;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{content, test_types};
    use penumbra_core::{NodePath, Value};

    /// A fresh directory under the system temp dir, removed again on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("penumbra-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const S: Duration = Duration::from_secs(1);

    fn open(dir: &TempDir) -> Session {
        let template = content(&[("/a", "test.const", &[], &[], false)]);
        Session::open(&dir.0, Arc::new(test_types()), &template).unwrap()
    }

    fn set(v: f64) -> Op {
        Op::Set { param: "/a.value".parse().unwrap(), value: Value::Float(v) }
    }

    fn value(s: &Session) -> Option<Value> {
        let net = s.doc().snapshot();
        let a = net.resolve(&"/a".parse::<NodePath>().unwrap())?;
        net.node(a)?.params.get("value").cloned()
    }

    fn file(s: &Session) -> String {
        std::fs::read_to_string(s.file()).unwrap()
    }

    #[test]
    fn a_new_project_is_written_from_the_template_and_reopens() {
        let dir = TempDir::new("template");
        let s = open(&dir);
        assert!(file(&s).contains(r#""/a": (kind: "test.const"),"#), "{}", file(&s));
        assert!(!s.status(Instant::now()).unsaved);
        drop(s);
        let again = open(&dir);
        assert_eq!(again.doc().snapshot().len(), 1);
    }

    #[test]
    fn autosave_waits_a_second_then_writes() {
        let dir = TempDir::new("autosave");
        let s = open(&dir);
        let t0 = Instant::now();
        s.edit(Actor::User, &[set(2.0)], None, "", t0).unwrap();
        s.poll(t0);
        assert!(!file(&s).contains("2.0"));
        assert!(s.status(t0).unsaved);
        s.poll(t0 + S);
        assert!(file(&s).contains(r#"params: {"value": 2.0}"#), "{}", file(&s));
        assert!(!s.status(t0 + S).unsaved);
        // Undo back to the written state: written again, so the file follows undo.
        s.undo(Actor::User, t0 + 2 * S).unwrap();
        s.poll(t0 + 2 * S);
        s.poll(t0 + 3 * S);
        assert!(!file(&s).contains("2.0"));
    }

    #[test]
    fn an_external_edit_is_merged_as_one_entry_and_keeps_ids() {
        let dir = TempDir::new("merge");
        let s = open(&dir);
        let id = s.doc().snapshot().resolve(&"/a".parse().unwrap());
        let t0 = Instant::now();
        let text =
            file(&s).replace(r#"(kind: "test.const")"#, r#"(kind: "test.const", params: {"value": 5})"#);
        std::fs::write(s.file(), &text).unwrap();
        s.poll(t0);
        assert_eq!(value(&s), Some(Value::Float(5.0)));
        assert_eq!(s.doc().snapshot().resolve(&"/a".parse().unwrap()), id);
        s.doc().with_history(|h| {
            assert_eq!(h.entries().len(), 1);
            assert_eq!(h.entries()[0].source, Source::ProjectFile);
        });
        // Then rewritten in canonical form.
        s.poll(t0 + S);
        s.poll(t0 + 2 * S);
        assert!(file(&s).contains(r#"{"value": 5.0}"#), "{}", file(&s));
        s.doc().with_history(|h| assert_eq!(h.entries().len(), 1));
    }

    #[test]
    fn a_file_edit_waits_while_the_user_edits() {
        let dir = TempDir::new("wait");
        let s = open(&dir);
        let t0 = Instant::now();
        s.edit(Actor::User, &[set(1.0)], None, "", t0).unwrap();
        let text = file(&s).replace(r#""/a""#, r#""/b": (kind: "test.const"), "/a""#);
        std::fs::write(s.file(), &text).unwrap();
        s.poll(t0 + S);
        assert!(s.status(t0 + S).merge_waiting);
        assert_eq!(s.doc().snapshot().len(), 1);
        // Autosave must not overwrite the waiting edit.
        s.poll(t0 + 2 * S);
        assert_eq!(file(&s), text);
        s.poll(t0 + 3 * S);
        assert_eq!(s.doc().snapshot().len(), 2);
        // The user's unsaved value survived the merge.
        assert_eq!(value(&s), Some(Value::Float(1.0)));
    }

    #[test]
    fn a_broken_file_pauses_autosave_until_it_is_fixed() {
        let dir = TempDir::new("broken");
        let s = open(&dir);
        let t0 = Instant::now();
        let good = file(&s);
        std::fs::write(s.file(), "(version: 1, nodes: {\"/a\": (kind: \"nope.x\")})").unwrap();
        s.poll(t0);
        let err = s.status(t0).file_error.unwrap();
        assert!(err.contains("unknown node kind"), "{err}");
        s.edit(Actor::User, &[set(3.0)], None, "", t0).unwrap();
        s.poll(t0 + 5 * S);
        assert!(file(&s).contains("nope.x"), "the broken file is someone's work; keep it");

        let fixed =
            good.replace(r#"(kind: "test.const")"#, r#"(kind: "test.const", params: {"value": 4.0})"#);
        std::fs::write(s.file(), &fixed).unwrap();
        s.poll(t0 + 9 * S);
        assert_eq!(s.status(t0 + 9 * S).file_error, None);
        assert_eq!(value(&s), Some(Value::Float(4.0)));
    }

    #[test]
    fn a_project_that_does_not_load_opens_empty_and_loads_once_fixed() {
        let dir = TempDir::new("badopen");
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join(FILE_NAME), "(version: 1, nodes: {\"/a\": (kind: \"test.const\",").unwrap();
        let s = open(&dir);
        assert!(s.doc().snapshot().is_empty());
        assert!(s.status(Instant::now()).file_error.is_some());
        s.poll(Instant::now());
        assert!(file(&s).ends_with(','), "not overwritten");
        std::fs::write(s.file(), "(version: 1, nodes: {\"/a\": (kind: \"test.const\")})").unwrap();
        s.poll(Instant::now());
        assert_eq!(s.doc().snapshot().len(), 1);
    }

    #[test]
    fn a_param_a_node_type_lost_does_not_block_the_project() {
        // A built-in that dropped a param (tex.composite's opacity, 2026-10-08) must not keep
        // the project from opening or merging; the value stays until it is removed.
        let dir = TempDir::new("stray");
        std::fs::create_dir_all(&dir.0).unwrap();
        let text = "(version: 1, nodes: {\"/a\": (kind: \"test.const\", params: {\"opacity\": 0.7})})";
        std::fs::write(dir.0.join(FILE_NAME), text).unwrap();
        let s = open(&dir);
        assert_eq!(s.status(Instant::now()).file_error, None);
        let net = s.doc().snapshot();
        let a = net.resolve(&"/a".parse::<NodePath>().unwrap()).unwrap();
        assert_eq!(net.node(a).unwrap().params.get("opacity"), Some(&Value::Float(0.7)));
        // Merged edits keep it too; edits from the UI may remove it.
        std::fs::write(s.file(), text.replace("0.7", "0.5")).unwrap();
        s.poll(Instant::now());
        assert_eq!(s.status(Instant::now()).file_error, None);
        let reset = Op::Reset { param: "/a.opacity".parse().unwrap() };
        s.edit(Actor::User, &[reset], None, "", Instant::now()).unwrap();
        let net = s.doc().snapshot();
        assert!(net.node(a).unwrap().params.is_empty());
    }

    #[test]
    fn save_overwrites_a_broken_file_on_request() {
        let dir = TempDir::new("save");
        let s = open(&dir);
        std::fs::write(s.file(), "garbage").unwrap();
        s.poll(Instant::now());
        assert!(s.status(Instant::now()).file_error.is_some());
        assert_eq!(s.save(), Ok(true));
        assert!(file(&s).contains("test.const"));
        assert_eq!(s.status(Instant::now()).file_error, None);
    }

    #[test]
    fn a_deleted_file_is_written_again() {
        let dir = TempDir::new("deleted");
        let s = open(&dir);
        let t0 = Instant::now();
        std::fs::remove_file(s.file()).unwrap();
        s.poll(t0);
        s.poll(t0 + S);
        assert!(file(&s).contains("test.const"));
    }

    #[test]
    fn claude_edits_need_quiet_and_join_the_claim() {
        let dir = TempDir::new("claim");
        let s = open(&dir);
        let t0 = Instant::now();
        s.edit(Actor::User, &[set(1.0)], None, "", t0).unwrap();
        assert!(s.edit(Actor::Claude, &[set(2.0)], None, "", t0 + S).is_err());
        assert!(s.begin("tuning", t0 + S).is_err());
        s.begin("tuning", t0 + 4 * S).unwrap();
        assert!(s.edit(Actor::User, &[set(9.0)], None, "", t0 + 4 * S).is_err());
        s.edit(Actor::Claude, &[set(2.0)], None, "", t0 + 5 * S).unwrap();
        s.edit(Actor::Claude, &[set(3.0)], None, "", t0 + 6 * S).unwrap();
        s.doc().with_history(|h| assert_eq!(h.entries().len(), 2, "the user's edit and Claude's claim"));
        assert!(s.end(t0 + 7 * S));
        s.edit(Actor::User, &[set(4.0)], None, "", t0 + 7 * S).unwrap();
    }
}
