//! The editor window: node editor, param panel, viewer, change log, status bar, and the UI side of
//! the `ui.*` commands.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, Key, Modifiers, Pos2, Rect, RichText, Sense, Ui, Vec2, vec2};
use penumbra_core::NodeId;
use penumbra_engine::{Engine, ViewFrame, ViewRequest};
use penumbra_graph::Port;
use penumbra_project::{Actor, Session, SessionStatus};
use serde_json::json;

use crate::changes::Changes;
use crate::control::{Reply, UiRequest};
use crate::editor::{self, Editor};
use crate::icons::{self, Icon};
use crate::layout::Layout;
use crate::navigate::{self, Move};
use crate::params::{self, ParamPanel};
use crate::textures::Textures;
use crate::viewer::Viewer;
use crate::{Action, automation, theme};

mod commands;

/// How long a screenshot may take to arrive.
const SHOT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an edit error stays in the status bar.
const MESSAGE_SECONDS: u64 = 8;
/// Resizes [`PenumbraApp::heal_surface`] tries within [`HEAL_WINDOW`] before it gives up. A GPU
/// kept busy or full by another program (ComfyUI, for one) fails every reconfigure, and each
/// nudge caused the next failure: a resize about once a minute for as long as it lasted.
const MAX_HEALS: u32 = 3;
const HEAL_WINDOW: Duration = Duration::from_secs(300);
/// How long after the start a window resize still counts as the window settling.
const SETTLE: Duration = Duration::from_secs(3);
/// How long penumbra's GPU work waits after a window's size changed, so egui's configures
/// of the following sizes find the GPU idle (see [`penumbra_gpu::Gpu::hold`]).
const RESIZE_HOLD: Duration = Duration::from_millis(250);
/// How long a window's size must stay put before a failed configure is healed: a nudge while
/// the user still drags the border fought the drag and took the focus.
const RESIZE_SETTLE: Duration = Duration::from_millis(500);
/// Height of a collapsed section of the right column, header only.
const SECTION_HEADER: f32 = 30.0;
/// The viewer's header row and frame (width, height) around its picture area, until the first
/// frame has measured them.
const VIEWER_OVERHEAD: Vec2 = vec2(12.0, 28.0);

/// What the app is started with besides the engine.
pub struct AppOptions {
    /// Shown in the status bar: the control channel address or why it is not available.
    pub control: Result<u16, String>,
    /// Where `ui.*` requests arrive ([`crate::control::Bridge::new`]).
    pub requests: Option<Receiver<UiRequest>>,
    pub bridge: Option<Arc<crate::control::Bridge>>,
    /// Open the 3D mouse (not in headless runs, which must not grab devices).
    pub spacenav: bool,
}

struct Shot {
    token: u64,
    file: PathBuf,
    reply: Sender<Reply>,
    /// Frames to draw before asking, so the picture is settled.
    settle: u32,
    asked: bool,
    deadline: Instant,
}

pub struct PenumbraApp {
    engine: Arc<Engine>,
    control: Result<u16, String>,
    adapter: String,
    textures: Textures,
    layout: Layout,
    editor: Editor,
    params: ParamPanel,
    viewer: Viewer,
    /// The viewer's window of its own: separate zoom, output choice and editors.
    window_viewer: Viewer,
    /// Whether the viewer's window was shown last frame; if not, this frame creates its surface.
    window_viewer_shown: bool,
    /// The viewer's panel and its picture area last frame, for the height of its header rows.
    viewer_frame: Option<(Rect, Rect)>,
    /// Whether the pointer went down on the viewer's edge and has not been released since.
    viewer_drag: bool,
    changes: Changes,
    auto: automation::Registry,
    requests: Option<Receiver<UiRequest>>,
    synthetic: VecDeque<egui::Event>,
    input_waiters: Vec<Sender<Reply>>,
    shots: Vec<Shot>,
    next_token: u64,
    message: Option<(String, Instant)>,
    /// The editing mode the visuals were last set for (`true`: Claude's blue).
    themed: Option<bool>,
    /// The editing mode the native title bar was last coloured for, and the window title last
    /// sent.
    title_bar: Option<bool>,
    title: String,
    gpu: Option<penumbra_gpu::Gpu>,
    /// Surface reconfigure failures already answered, and the resize in progress.
    surface_failures: u64,
    nudge: Option<(egui::ViewportId, Nudge)>,
    /// A failed configure not answered yet: no window was resized since.
    heal_pending: bool,
    /// When the app started: the editor is framed again while the window settles after that.
    started: Instant,
    /// The sizes of the editor's and the viewer's window last seen, and when and which one
    /// last changed.
    sizes: [Option<egui::Vec2>; 2],
    resized: Option<(Instant, egui::ViewportId)>,
    /// Nudges since the window was last quiet for [`HEAL_WINDOW`], and when the last one began.
    heals: (u32, Option<Instant>),
    helpers: crate::helpers::Helpers,
    /// The window's place on the desktop as last told to the outputs.
    editor_rect: Option<[i32; 4]>,
    /// Component files on offer in the add dialog, and when they were last listed.
    library: (Vec<crate::add_dialog::Offered>, Option<Instant>),
    /// What this run copied last, and text waiting to go to the system clipboard.
    copied: Option<crate::clipboard::Copied>,
    to_clipboard: Option<String>,
    /// The folder button with the project menu.
    projects: crate::projects::ProjectMenu,
    /// The project shown, and the engine's generation it belongs to: when another project
    /// opens, the editor starts over for it ([`PenumbraApp::project_switched`]).
    shown: (u64, Arc<Session>),
    /// When the 3D mouse was last applied, for the time its deflection was held.
    flown: Instant,
    /// The open project is the demo or an example, made afresh on every open.
    starts_fresh: bool,
    /// The `?` buttons may open the browser ([`crate::control::Bridge::allow_browser`]).
    browser: bool,
}

/// Resizing the window once more so egui-wgpu reconfigures a surface whose last configure
/// failed (see [`penumbra_gpu::ErrorLog::surface_configure_failures`]): the cook thread shares
/// the device and may submit while wgpu waits for an idle GPU during a resize. Two frames: away
/// and back.
enum Nudge {
    Restore(egui::Vec2),
    Remaximize,
}

impl PenumbraApp {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: Arc<Engine>, options: AppOptions) -> Self {
        // Maximized by command once the window exists: as a creation flag, Windows left the
        // window at its inner size, in the corner and without a frame.
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        let mut shared_gpu = None;
        let adapter = match cc.wgpu_render_state.as_ref() {
            Some(rs) => {
                // One device for the UI and the cook thread, so textures need no copies.
                let gpu = penumbra_gpu::Gpu::new(rs.adapter.clone(), rs.device.clone(), rs.queue.clone());
                let (warm, types) = (gpu.clone(), engine.types().clone());
                let spawned = std::thread::Builder::new()
                    .name("shader-warm-up".into())
                    .spawn(move || penumbra_ops::warm_up(&types, &warm));
                if let Err(e) = spawned {
                    log::warn!("cannot precompile the built-in shaders: {e}");
                }
                if let Err(e) = engine.attach_gpu(gpu.clone()) {
                    log::error!("{e}");
                }
                shared_gpu = Some(gpu.clone());
                gpu.name()
            }
            None => "no wgpu adapter".into(),
        };
        log::info!("GPU: {adapter}");
        if let Some(bridge) = &options.bridge {
            bridge.attach(cc.egui_ctx.clone());
        }
        if options.spacenav {
            engine.spacenav().start();
        }
        let layout = Layout::load(Some(engine.session().dir()));
        cc.egui_ctx.set_visuals(theme::visuals(false));
        Self {
            control: options.control,
            adapter,
            textures: Textures::new(cc.wgpu_render_state.clone()),
            layout,
            editor: Editor::default(),
            params: ParamPanel::default(),
            viewer: Viewer::default(),
            window_viewer: Viewer::default(),
            window_viewer_shown: false,
            viewer_frame: None,
            viewer_drag: false,
            changes: Changes::default(),
            auto: automation::Registry::default(),
            requests: options.requests,
            synthetic: VecDeque::new(),
            input_waiters: Vec::new(),
            shots: Vec::new(),
            next_token: 1,
            message: None,
            themed: None,
            title_bar: None,
            title: String::new(),
            gpu: shared_gpu,
            surface_failures: 0,
            nudge: None,
            heal_pending: false,
            started: Instant::now(),
            sizes: [None; 2],
            resized: None,
            heals: (0, None),
            helpers: crate::helpers::Helpers::default(),
            editor_rect: None,
            library: (Vec::new(), None),
            copied: None,
            to_clipboard: None,
            projects: crate::projects::ProjectMenu::default(),
            shown: (engine.generation(), engine.session()),
            flown: Instant::now(),
            starts_fresh: penumbra_engine::projects::starts_fresh(engine.session().dir()),
            browser: options.bridge.as_ref().is_some_and(|b| b.browser()),
            engine,
        }
    }

    /// Another project opened: writes the old one's layout, then starts over with the new
    /// one's (selection, viewers, change log, texture cache; node ids are not comparable across
    /// projects). The clipboard, the Helpers window and the window itself stay.
    fn project_switched(&mut self, now: Instant) {
        let (generation, old) = (self.engine.generation(), self.engine.session());
        let (_, previous) = std::mem::replace(&mut self.shown, (generation, old.clone()));
        // The same folder again means it was made afresh (the demo or an example opened anew):
        // the old layout must not be written over the fresh one.
        if !penumbra_engine::projects::same_dir(previous.dir(), old.dir()) {
            self.layout.save_if_due(now, true);
        }
        self.starts_fresh = penumbra_engine::projects::starts_fresh(old.dir());
        self.layout = Layout::load(Some(old.dir()));
        self.editor = Editor::default();
        self.params = ParamPanel::default();
        self.viewer = Viewer::default();
        self.window_viewer = Viewer::default();
        self.changes = Changes::default();
        self.textures.clear();
        self.library.1 = None;
        self.title.clear();
    }

    /// Writes the layout now; for runs that end without `on_exit` (headless).
    pub fn close(&mut self) {
        self.layout.save_if_due(Instant::now(), true);
    }

    /// Synthetic input queued by `ui.click` and friends, one pointer event per frame. The
    /// headless runner feeds these to its harness; the window takes them in `raw_input_hook`.
    pub fn take_synthetic(&mut self) -> Option<egui::Event> {
        self.synthetic.pop_front()
    }

    fn selected(&self) -> Option<NodeId> {
        match self.editor.selection().iter().copied().collect::<Vec<_>>().as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    fn apply(&mut self, actions: Vec<Action>, now: Instant) {
        let session = self.engine.session().clone();
        for action in actions {
            let result = match action {
                Action::Edit { ops, label, group } => {
                    session.edit(Actor::User, &ops, group, &label, now).map(|out| {
                        if let Some(id) = out.created.first() {
                            self.editor.select([*id]);
                        }
                    })
                }
                Action::Undo => self.undo(now),
                Action::Redo => self.redo(now),
                Action::TakeOver => {
                    session.take_over();
                    Ok(())
                }
                Action::SelectNodes(set) => {
                    self.editor.select(set);
                    Ok(())
                }
                Action::Pin(id) => {
                    self.viewer.pinned = id;
                    Ok(())
                }
                Action::Play(playing) => {
                    self.engine.play(playing);
                    Ok(())
                }
                Action::OpenHelpers => {
                    self.helpers.open();
                    Ok(())
                }

                Action::Message(m) => Err(penumbra_core::Error::new(m)),
                Action::Group(ids) => {
                    crate::components::group(&self.engine, &mut self.editor, &self.layout, &ids, now)
                }
                Action::Dissolve(id) => {
                    crate::components::dissolve(&self.engine, &mut self.editor, &self.layout, id, now)
                }
                Action::SaveComponent(id) => crate::components::save(&self.engine, id).map(|text| {
                    self.message = Some((text, now));
                    self.library.1 = None;
                }),
                Action::RemoveComponent(name) => crate::components::remove(&self.engine, &name).map(|text| {
                    self.message = Some((text, now));
                    self.library.1 = None;
                }),
                Action::AddComponent { name, level, at } => {
                    crate::components::add(&self.engine, &mut self.editor, &name, level, at, now)
                }
                Action::Copy(ids) => {
                    crate::clipboard::copy(&self.engine, &ids).map(|c| self.copied_now(c, now))
                }
                Action::Cut(ids) => {
                    crate::clipboard::cut(&self.engine, &ids).map(|c| self.copied_now(c, now))
                }
                Action::Paste { text, level, at } => crate::clipboard::paste(
                    &self.engine,
                    &mut self.editor,
                    self.copied.as_ref(),
                    text.as_deref(),
                    level,
                    at,
                    now,
                )
                .map(|message| {
                    if let Some(m) = message {
                        self.message = Some((m, now));
                    }
                }),
                Action::Replace { node, kind } => self.replace(node, &kind, now),
                Action::Reset(ids) => self.engine.reset_nodes(&ids).map(|report| {
                    self.message = Some((report.message(), now));
                }),
                Action::LearnBackground { node, delay } => {
                    let net = self.engine.session().doc().snapshot();
                    match net.path_of(node) {
                        Some(path) => penumbra_engine::blob_track::learn(&self.engine, &path, delay)
                            .map(|_| {
                                let text = if delay > 0.0 {
                                    format!("the background is taken in {delay:.0} s")
                                } else {
                                    "the background is taken from the next frame".to_string()
                                };
                                self.message = Some((text, now));
                            })
                            .map_err(|e| penumbra_core::Error::new(e.0)),
                        None => Err(penumbra_core::Error::new("the node is gone")),
                    }
                }
                Action::Trace { node, from } => {
                    let net = self.engine.session().doc().snapshot();
                    match net.path_of(node) {
                        Some(path) => {
                            let args = penumbra_engine::shapes::TraceArgs::new(path, &from);
                            penumbra_engine::shapes::trace_into(&self.engine, Actor::User, &args, now)
                                .map(|r| {
                                    let text = r
                                        .get("message")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("traced")
                                        .to_string();
                                    let warnings: Vec<&str> = r
                                        .get("warnings")
                                        .and_then(|w| w.as_array())
                                        .map(|w| w.iter().filter_map(|v| v.as_str()).collect())
                                        .unwrap_or_default();
                                    let text = if warnings.is_empty() {
                                        text
                                    } else {
                                        format!("{text}; {}", warnings.join("; "))
                                    };
                                    self.message = Some((text, now));
                                })
                                .map_err(|e| penumbra_core::Error::new(e.0))
                        }
                        None => Err(penumbra_core::Error::new("the node is gone")),
                    }
                }
                Action::Scan(node) => {
                    let nodes = penumbra_engine::scan_run::rig_of(&self.engine, node);
                    let args = penumbra_engine::scan_run::RunArgs { nodes: Some(nodes), wait: None };
                    penumbra_engine::scan_run::start(&self.engine, &args, Actor::User)
                        .map(|r| {
                            let text =
                                r.get("message").and_then(|m| m.as_str()).unwrap_or("scanning").to_string();
                            self.message = Some((text, now));
                        })
                        .map_err(|e| penumbra_core::Error::new(e.0))
                }
                Action::CancelScan => {
                    if self.engine.scans().cancel() {
                        self.message = Some(("the scan stops before its next picture".into(), now));
                    }
                    Ok(())
                }
                Action::Help(kind) => crate::help::open(Some(&kind), self.browser)
                    .map(|reply| {
                        let text = if reply.get("opened").and_then(|o| o.as_bool()) == Some(true) {
                            format!("the documentation of {kind} opens in the browser")
                        } else {
                            format!(
                                "the documentation of {kind}: {}",
                                reply.get("url").and_then(|u| u.as_str()).unwrap_or_default()
                            )
                        };
                        self.message = Some((text, now));
                    })
                    .map_err(penumbra_core::Error::new),
                Action::LearnOsc(param) => {
                    let timeout = penumbra_engine::osc::LEARN_TIMEOUT;
                    match self.engine.osc().learn(&self.engine.session(), &param, timeout) {
                        Ok(()) => {
                            let text = format!(
                                "{param}: move a control on the OSC device ({} s; Helpers > OSC to cancel)",
                                timeout.as_secs()
                            );
                            self.message = Some((text, now));
                            Ok(())
                        }
                        Err(e) => Err(penumbra_core::Error::new(e)),
                    }
                }
            };
            if let Err(e) = result {
                log::warn!("{e}");
                self.message = Some((e.0, now));
            }
        }
    }

    /// Replaces `node` with a new node of `kind` (`node.replace`, as the user): the new one takes
    /// the old one's place and the selection; what could not be kept goes to the status bar.
    fn replace(&mut self, node: NodeId, kind: &str, now: Instant) -> Result<(), penumbra_core::Error> {
        let session = self.engine.session().clone();
        let net = session.doc().snapshot();
        let path = net.path_of(node).ok_or_else(|| penumbra_core::Error::new("the node is gone"))?;
        let r = penumbra_graph::replace(&net, self.engine.types(), &path, kind)?;
        let label = format!("replace {path} with {kind}");
        let out = session.edit(Actor::User, &r.ops, None, &label, now)?;
        if let Some(id) = out.created.first() {
            self.editor.select([*id]);
        }
        let text = if r.lost.is_empty() {
            format!("{path} is a {kind} now")
        } else {
            format!("{path} is a {kind} now; not kept: {}", r.lost.join(", "))
        };
        self.message = Some((text, now));
        Ok(())
    }

    /// Keeps what was just copied for pasting and hands its text to the system clipboard.
    fn copied_now(&mut self, copied: crate::clipboard::Copied, now: Instant) {
        self.message = Some((format!("copied {} node(s)", copied.count), now));
        self.to_clipboard = Some(copied.text.clone());
        self.copied = Some(copied);
    }

    /// Undoes the newest history entry (moves of nodes included).
    fn undo(&mut self, now: Instant) -> Result<(), penumbra_core::Error> {
        self.engine.session().undo(Actor::User, now).map(|_| ())
    }

    /// Redoes the oldest undone history entry.
    fn redo(&mut self, now: Instant) -> Result<(), penumbra_core::Error> {
        self.engine.session().redo(Actor::User, now).map(|_| ())
    }

    fn global_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Over the region editor's picture, undo and redo step through its own changes: they
        // are files next to the scene, not edits of the network.
        if ctx.text_edit_focused() || self.viewer.regions.has_pointer(ctx) {
            return;
        }
        let (undo, redo) = ctx.input_mut(|i| {
            let redo = i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
                || i.consume_key(Modifiers::COMMAND, Key::Y);
            (i.consume_key(Modifiers::COMMAND, Key::Z), redo)
        });
        if undo {
            actions.push(Action::Undo);
        }
        if redo {
            actions.push(Action::Redo);
        }
    }

    /// Who is editing goes into the window title; a bar on top only while there is more to say:
    /// Claude's claim with its countdown and the take-over button, file and autosave errors
    /// (a whole line for "You are editing" was too much).
    fn top_bar(&mut self, ui: &mut Ui, status: &SessionStatus, actions: &mut Vec<Action>) {
        let claude = status.edit.mode == "claude";
        let mode = if claude {
            let label = status.edit.label.as_deref().filter(|l| !l.is_empty()).unwrap_or("working");
            format!("Claude is editing: {label}")
        } else {
            "you are editing".to_string()
        };
        let title = window_title(self.engine.session().dir(), &mode);
        if title != self.title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if !claude
            && status.file_error.is_none()
            && status.write_error.is_none()
            && !status.merge_waiting
            && status.broken.is_empty()
        {
            return;
        }
        egui::Panel::top("mode").show(ui, |ui| {
            ui.horizontal(|ui| {
                if claude {
                    let label = status.edit.label.as_deref().filter(|l| !l.is_empty()).unwrap_or("working");
                    ui.label(
                        RichText::new(format!("Claude is editing: {label}")).color(theme::CLAUDE).strong(),
                    );
                    if let Some(s) = status.edit.expires_in {
                        ui.label(
                            RichText::new(format!("(lapses in {s:.0} s without a command)"))
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    let r = icons::button(ui, Icon::TakeOver, "Take over");
                    self.auto.add("button.take_over", r.rect, "Take over");
                    if r.clicked() {
                        actions.push(Action::TakeOver);
                    }
                }
                if let Some(e) = &status.file_error {
                    if claude {
                        ui.separator();
                    }
                    ui.colored_label(theme::ERROR, format!("project file not merged: {e}"));
                }
                if let Some(e) = &status.write_error {
                    if claude || status.file_error.is_some() {
                        ui.separator();
                    }
                    ui.colored_label(theme::ERROR, format!("autosave failed: {e}"));
                }
                if status.merge_waiting {
                    if claude || status.file_error.is_some() || status.write_error.is_some() {
                        ui.separator();
                    }
                    ui.colored_label(theme::FILE, "a file edit waits until you pause");
                }
                if !status.broken.is_empty() {
                    if claude
                        || status.file_error.is_some()
                        || status.write_error.is_some()
                        || status.merge_waiting
                    {
                        ui.separator();
                    }
                    let paths: Vec<&str> = status.broken.iter().map(|b| b.path.as_str()).collect();
                    let r = ui.colored_label(
                        theme::ERROR,
                        format!("did not load, kept as written in the project file: {}", paths.join(", ")),
                    );
                    let why: Vec<String> = status
                        .broken
                        .iter()
                        .map(|b| format!("{} ({}): {}", b.path, b.kind, b.error))
                        .collect();
                    r.on_hover_text(why.join("\n"));
                }
            });
        });
    }

    fn status_bar(
        &mut self,
        ui: &mut Ui,
        status: &SessionStatus,
        view: &ViewFrame,
        board: &penumbra_ops::out::BoardState,
        actions: &mut Vec<Action>,
        now: Instant,
    ) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let layout = &mut self.layout;
                let save_layout = || layout.save_if_due(now, true);
                if let Some(m) = self.projects.show(ui, &self.engine, &mut self.auto, save_layout) {
                    self.message = Some((m, now));
                }
                let playing = view.playing;
                let r = if playing {
                    icons::button(ui, Icon::Pause, "Pause")
                } else {
                    icons::button(ui, Icon::Play, "Play")
                };
                self.auto.add("button.play", r.rect, "Play/Pause");
                if r.clicked() {
                    actions.push(Action::Play(!playing));
                }
                if let Some(t) = view.time {
                    ui.monospace(format!("{:>7}  {:>8.2} s", t.frame, t.seconds()));
                }
                let fps = view.time.map_or(60.0, |t| t.rate.as_f64());
                crate::cook_meter::status(ui, &self.engine, fps, &mut self.auto);
                ui.separator();
                self.helpers.status(ui, board, view.pace, &mut self.auto);
                ui.separator();
                match &self.control {
                    Ok(port) => {
                        ui.label(RichText::new(format!("control 127.0.0.1:{port}")).color(theme::TEXT_WEAK))
                    }
                    Err(e) => ui.colored_label(theme::ERROR, format!("control channel: {e}")),
                };
                ui.separator();
                ui.label(RichText::new(&self.adapter).color(theme::TEXT_WEAK));
                ui.separator();
                ui.label(
                    RichText::new(if status.unsaved { "unsaved" } else { "saved" }).color(theme::TEXT_WEAK),
                );
                if self.starts_fresh {
                    ui.label(RichText::new("until reopened").color(theme::TEXT_WEAK)).on_hover_text(
                        "The demo and the examples start fresh on every open. To keep your changes, \
                         use \"Save copy as\" in the project menu.",
                    );
                }
                if let Some(notice) = self.engine.notice() {
                    ui.separator();
                    ui.colored_label(theme::WARNING, notice);
                }
                if let Some((m, at)) = &self.message {
                    if now.saturating_duration_since(*at).as_secs() < MESSAGE_SECONDS {
                        ui.separator();
                        ui.colored_label(theme::WARNING, m);
                    } else {
                        self.message = None;
                    }
                }
            });
        });
    }

    fn frame(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        self.auto.begin_frame();
        if self.engine.generation() != self.shown.0 {
            self.project_switched(now);
        }
        // The viewers' channel scale lives in the layout; a click in either viewer's header
        // changes it there at the end of the frame.
        self.viewer.chan_scale = self.layout.options.chan_scale;
        self.window_viewer.chan_scale = self.layout.options.chan_scale;
        self.viewer.audio_view = self.layout.options.audio_view;
        self.window_viewer.audio_view = self.layout.options.audio_view;
        let session = self.engine.session();
        let status = session.status(now);
        let net = session.doc().snapshot();
        let view = self.engine.view();
        let board = self.engine.board().state();
        let can_edit = status.edit.mode != "claude";
        if self.themed != Some(!can_edit) {
            self.themed = Some(!can_edit);
            ctx.set_visuals(theme::visuals(!can_edit));
        }
        self.changes.refresh(session.doc());
        let recent = self.changes.recent(now);
        let mut actions = Vec::new();
        self.global_keys(&ctx, &mut actions);
        let window = ui.max_rect();

        self.top_bar(ui, &status, &mut actions);
        self.status_bar(ui, &status, &view, &board, &mut actions, now);

        let selected = self.selected();
        let detail =
            selected.and_then(|id| view.detail_params.as_ref().filter(|(d, _)| *d == id)).map(|(_, p)| p);
        // A wire dragged from a chan output can be dropped onto a param to bind it. Taken before
        // the editor draws, which ends the drag on release.
        let chan_drag = self.editor.wire_source().and_then(|(node, output)| {
            let kind = net
                .node(node)
                .and_then(|n| session.types().desc(&n.kind))?
                .outputs
                .get(output as usize)?
                .kind;
            (kind == penumbra_graph::DataKind::Chan).then_some(node)
        });
        let target = self.viewer.target(&net, selected);
        // One right column: params on top, then viewer and changes, each collapsible; the
        // editor gets everything else. The column keeps its share of the window (three tenths to
        // start with), so it grows and shrinks with the window: egui would keep its width in
        // points, so its stored size is set to the share's every frame. A width that comes out
        // different is the user dragging the edge and gives the new share. Resizes of the window
        // (it maximizes just after opening) never change the share.
        let window_width = ui.available_width();
        let column_width = crate::layout::column_width(self.layout.column, window_width);
        let column_id = egui::Id::new("column");
        let stored = egui::Rect::from_min_size(
            egui::pos2(window.right() - column_width, window.top()),
            vec2(column_width, window.height()),
        );
        ui.ctx().data_mut(|d| {
            d.insert_persisted(column_id, egui::containers::panel::PanelState { outer_rect: stored })
        });
        let column = egui::Panel::right(column_id)
            .resizable(true)
            .default_size(column_width)
            .min_size(crate::layout::COLUMN_MIN)
            .show(ui, |ui| {
                // The column is as wide as the user dragged it, never as its content: egui widens a
                // panel to whatever was drawn into it, so a node with a long row made the column
                // jump when selected. The content goes into a child that does not report its size back;
                // what does not fit is clipped.
                let rect = ui.max_rect();
                ui.expand_to_include_rect(rect);
                let mut column =
                    ui.new_child(egui::UiBuilder::new().id_salt("column_content").max_rect(rect));
                column.set_clip_rect(rect.intersect(ui.clip_rect()));
                let ui = &mut column;
                let mut sections = self.layout.sections;
                let changes_open = sections.changes;
                let panel = if changes_open {
                    egui::Panel::bottom("changes").resizable(true).default_size(200.0).min_size(90.0)
                } else {
                    egui::Panel::bottom("changes_closed").resizable(false).exact_size(SECTION_HEADER)
                };
                panel.show(ui, |ui| {
                    let (undo, redo) = (status.can_undo && can_edit, status.can_redo && can_edit);
                    section(ui, &mut self.auto, "changes", "Changes", &mut sections.changes, |ui, auto| {
                        crate::changes::buttons(ui, undo, redo, auto, &mut actions);
                    });
                    if changes_open {
                        self.changes.show(ui, &mut self.auto, &mut actions);
                    }
                });

                // The viewer is as tall as its picture area needs to be 16:9 at the column's
                // width (or the aspect its edge was dragged to), below header rows that come and
                // go with the node shown. egui would keep the height in points it had on the
                // first frame, when the window was still small, so the stored height is set every
                // frame, as for the column; header rows and frame are measured the frame before.
                // A drag of the edge keeps its aspect, a double-click on the edge (or
                // `ui.options {viewer_aspect: "auto"}`) goes back to 16:9. The parameters above
                // keep a minimum: in a low window the viewer is shorter instead.
                let viewer_open = sections.viewer;
                let viewer_id = egui::Id::new("viewer");
                let room = ui.available_rect_before_wrap();
                let grab = ui.style().interaction.resize_grab_radius_side;
                let (overhead, edge) = match self.viewer_frame {
                    Some((panel, picture)) => (
                        panel.size() - picture.size(),
                        Some(Rect::from_x_y_ranges(panel.x_range(), panel.top()..=panel.top()).expand(grab)),
                    ),
                    None => (VIEWER_OVERHEAD, None),
                };
                let on_edge = |p: Option<Pos2>| p.zip(edge).is_some_and(|(p, e)| e.contains(p));
                if viewer_open && let Some(edge) = edge {
                    self.auto.add("viewer.edge", edge, "viewer height: drag, double-click for 16:9");
                    let (pressed, double) = ui.input(|i| {
                        let on = on_edge(i.pointer.interact_pos());
                        (
                            i.pointer.primary_pressed() && on,
                            i.pointer.button_double_clicked(egui::PointerButton::Primary) && on,
                        )
                    });
                    self.viewer_drag |= pressed;
                    if double {
                        self.layout.set_viewer_aspect(None, now);
                    }
                }
                let aspect = self.layout.viewer_aspect.unwrap_or(crate::layout::VIEWER_ASPECT);
                let viewer_height = crate::layout::viewer_height(
                    aspect,
                    room.width() - overhead.x,
                    overhead.y,
                    room.height(),
                );
                let panel = if viewer_open {
                    let stored = Rect::from_min_size(
                        egui::pos2(room.left(), room.bottom() - viewer_height),
                        vec2(room.width(), viewer_height),
                    );
                    ui.ctx().data_mut(|d| {
                        d.insert_persisted(
                            viewer_id,
                            egui::containers::panel::PanelState { outer_rect: stored },
                        )
                    });
                    egui::Panel::bottom(viewer_id)
                        .resizable(true)
                        .default_size(viewer_height)
                        .min_size(crate::layout::VIEWER_MIN)
                        .max_size(crate::layout::viewer_max(room.height()))
                } else {
                    egui::Panel::bottom("viewer_closed").resizable(false).exact_size(SECTION_HEADER)
                };
                let shown = panel.show(ui, |ui| {
                    let outputs = target.and_then(|id| view.outputs.get(&id));
                    let doc = session.doc();
                    let new_group = || doc.new_group();
                    let (map, shapes, output_names) =
                        node_editors(&net, session.types(), &view, target, can_edit, &new_group);
                    let regions = region_inputs(&net, session.types(), target, session.dir(), can_edit);
                    let viewer = &mut self.viewer;
                    let scene =
                        target.and_then(|id| navigate::scene(&net, session.types(), id, viewer.output));
                    // Wrapping: in a narrow column the buttons go on in a second row rather than
                    // out of sight.
                    section_row(
                        ui,
                        true,
                        &mut self.auto,
                        "viewer",
                        "Viewer",
                        &mut sections.viewer,
                        |ui, auto| {
                            viewer.header(
                                ui,
                                &net,
                                target,
                                outputs,
                                &output_names,
                                false,
                                scene.as_ref(),
                                auto,
                                &mut actions,
                            );
                        },
                    );
                    if viewer_open {
                        let nav = scene.as_ref().map(|scene| crate::viewer::NavInputs {
                            scene,
                            free: free_picture(&view, scene.node, self.viewer.output, false),
                            net: &net,
                            types: session.types(),
                            can_edit,
                            new_group: &new_group,
                        });
                        self.viewer.show(
                            ui,
                            target,
                            outputs,
                            view.time,
                            theme::picture_bg(!can_edit),
                            &mut self.textures,
                            map.as_ref(),
                            shapes.as_ref(),
                            regions.as_ref(),
                            nav.as_ref(),
                            &mut self.auto,
                            &mut actions,
                        );
                    }
                });
                if viewer_open && let Some(picture) = self.viewer.picture {
                    // A height other than the one set is the user dragging the edge.
                    let shown = shown.response.rect;
                    if self.viewer_drag
                        && (shown.height() - viewer_height).abs() > 1.0
                        && let Some(aspect) = crate::layout::viewer_aspect(picture.size())
                    {
                        self.layout.set_viewer_aspect(Some(aspect), now);
                    }
                    self.viewer_frame = Some((shown, picture));
                }
                if !ui.input(|i| i.pointer.primary_down()) {
                    self.viewer_drag = false;
                }

                egui::CentralPanel::default().show(ui, |ui| {
                    section(ui, &mut self.auto, "params", "Parameters", &mut sections.params, |_, _| {});
                    if !sections.params {
                        return;
                    }
                    match selected {
                        Some(id) => {
                            let doc = session.doc();
                            let new_group = || doc.new_group();
                            let input = params::Inputs {
                                net: &net,
                                types: session.types(),
                                node: id,
                                status: view.status.get(&id),
                                instance: detail.map(Vec::as_slice),
                                inner: &view.inner_params,
                                can_edit,
                                project_dir: Some(session.dir()),
                                new_group: &new_group,
                                displays: &board.displays,
                                outputs: &view.outputs,
                                chan_drag,
                                cameras: self.engine.camera(),
                                audio: self.engine.audio(),
                                record: self.engine.record(),
                                scans: self.engine.scans(),
                                show_rate: view.time.map(|t| t.rate.as_f64()),
                            };
                            self.params.show(ui, &input, &mut self.auto, &mut actions);
                        }
                        None => {
                            let n = self.editor.selection().len();
                            let text = if n == 0 {
                                "Select a node to edit its parameters.".to_string()
                            } else {
                                format!("{n} nodes selected")
                            };
                            ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                        }
                    }
                });
                self.layout.set_sections(sections, now);
            });
        let shown = column.response.rect.width();
        if (shown - column_width).abs() > 1.0 && column_width < window_width {
            self.layout.set_column(crate::layout::column_share(shown, window_width), now);
        }

        // The library is listed again every few seconds (two small directories).
        if self.library.1.is_none_or(|t| now.saturating_duration_since(t) > Duration::from_secs(3)) {
            self.library = (crate::components::library_names(&self.engine), Some(now));
        }
        let period = view.time.map_or(1.0 / 60.0, |t| t.rate.period().as_secs_f64());
        let background = if can_edit { theme::BG_USER } else { theme::BG_CLAUDE };
        let (thumbnails, passive) = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let input = editor::Inputs {
                    net: &net,
                    types: session.types(),
                    status: &view.status,
                    outputs: &view.outputs,
                    pictures: &view.pictures,
                    thumbnail_mode: net.thumbnails().0,
                    thumbnail_every: net.thumbnails().1,
                    fps: view.time.map_or(60.0, |t| t.rate.as_f64()),
                    chan_bars: self.layout.options.chan_bars,
                    binding_lines: self.layout.options.binding_lines,
                    backdrop: target
                        .filter(|_| self.layout.options.viewer_background)
                        .map(|id| (id, self.viewer.output)),
                    period,
                    can_edit,
                    viewer: target,
                    highlight: &self.changes.hovered,
                    recent: &recent,
                    now,
                    library: &self.library.0,
                };
                self.editor.show(
                    ui,
                    &input,
                    &mut self.layout,
                    &mut self.textures,
                    &mut self.auto,
                    &mut actions,
                    background,
                )
            })
            .inner;
        // Nodes that came without a position keep the place the editor gave them.
        let placed = self.layout.take_placed();
        if !placed.is_empty()
            && let Err(e) = session.place_unplaced(&placed)
        {
            log::warn!("placing new nodes: {e}");
        }

        let helpers = crate::helpers::HelpersInput {
            board: &board,
            net: &net,
            view: &view,
            engine: &self.engine,
            can_edit,
        };
        self.helpers.show(&ctx, &helpers, &mut self.auto, &mut actions);
        self.viewer_window(&ctx, &net, &view, can_edit, &mut actions);
        self.fly(&ctx, &net, &view, can_edit, now, &mut actions);
        let scale = self.layout.options.chan_scale;
        if let Some(changed) =
            [self.viewer.chan_scale, self.window_viewer.chan_scale].into_iter().find(|s| *s != scale)
        {
            let options = crate::layout::EditorOptions { chan_scale: changed, ..self.layout.options };
            self.layout.set_options(options, now);
        }
        let audio = self.layout.options.audio_view;
        if let Some(changed) =
            [self.viewer.audio_view, self.window_viewer.audio_view].into_iter().find(|v| *v != audio)
        {
            let options = crate::layout::EditorOptions { audio_view: changed, ..self.layout.options };
            self.layout.set_options(options, now);
        }
        self.apply(actions, now);
        if let Some(text) = self.to_clipboard.take() {
            ctx.copy_text(text);
        }
        let main = self.viewer.target(&net, self.selected());
        let mut viewers: Vec<NodeId> = main.into_iter().collect();
        viewers.extend(self.viewer.window.filter(|id| !viewers.contains(id)));
        let shown: Vec<Port> = main
            .map(|node| Port { node, output: self.viewer.output })
            .into_iter()
            .chain(self.viewer.window.map(|node| Port { node, output: self.window_viewer.output }))
            .collect();
        let mut free = Vec::new();
        let types = session.types();
        if self.layout.sections.viewer
            && let Some(scene) = main.and_then(|id| navigate::scene(&net, types, id, self.viewer.output))
        {
            free.extend(self.viewer.free_request(&scene));
        }
        if let Some(scene) =
            self.viewer.window.and_then(|id| navigate::scene(&net, types, id, self.window_viewer.output))
        {
            free.extend(self.window_viewer.free_request(&scene));
        }
        self.engine.set_view(ViewRequest {
            viewers,
            shown,
            thumbnails,
            passive,
            detail: self.selected(),
            resolution: self.helpers.open,
            free,
        });
        self.textures.end_frame();
        self.layout.save_if_due(now, false);
        // A line under the title bar, like the one the status bar draws above itself. Painted last,
        // over whatever panel sits at the top.
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().hline(window.x_range(), window.top() + stroke.width / 2.0, stroke);
    }

    /// The viewer's window (`viewer.window`): a node's output in a window of its own, larger
    /// than the viewer, with the mapper's or the shapes' editor where the node has one. Stays on
    /// the node it was opened for; closes when that node goes away.
    fn viewer_window(
        &mut self,
        ctx: &egui::Context,
        net: &penumbra_graph::Network,
        view: &penumbra_engine::ViewFrame,
        can_edit: bool,
        actions: &mut Vec<Action>,
    ) {
        let opening = !std::mem::replace(&mut self.window_viewer_shown, false);
        let Some(id) = self.viewer.window else { return };
        if net.node(id).is_none() {
            self.viewer.window = None;
            return;
        }
        self.window_viewer_shown = true;
        let session = self.engine.session().clone();
        let doc = session.doc();
        let new_group = || doc.new_group();
        let title = format!("Viewer {}", net.path_of(id).map(|p| p.to_string()).unwrap_or_default());
        let builder =
            egui::ViewportBuilder::default().with_title(title.clone()).with_inner_size([1280.0, 800.0]);
        let (window, textures, auto) = (&mut self.window_viewer, &mut self.textures, &mut self.auto);
        let mut close = false;
        let background = theme::picture_bg(!can_edit);
        let frame = view.time;
        let mut content = |ui: &mut Ui| {
            let (map, shapes, names) =
                node_editors(net, session.types(), view, Some(id), can_edit, &new_group);
            let regions = region_inputs(net, session.types(), Some(id), session.dir(), can_edit);
            let outputs = view.outputs.get(&id);
            let scene = navigate::scene(net, session.types(), id, window.output);
            ui.horizontal(|ui| {
                window.header(ui, net, Some(id), outputs, &names, true, scene.as_ref(), auto, actions)
            });
            let nav = scene.as_ref().map(|scene| crate::viewer::NavInputs {
                scene,
                free: free_picture(view, id, window.output, true),
                net,
                types: session.types(),
                can_edit,
                new_group: &new_group,
            });
            window.show(
                ui,
                Some(id),
                outputs,
                frame,
                background,
                textures,
                map.as_ref(),
                shapes.as_ref(),
                regions.as_ref(),
                nav.as_ref(),
                auto,
                actions,
            );
        };
        let gpu = self.gpu.as_ref().filter(|_| opening);
        let mut size = None;
        let keyboard = self.engine.keyboard().clone();
        let show = || {
            ctx.show_viewport_immediate(viewer_viewport(), builder, |ctx, class| {
                size = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
                if class != egui::ViewportClass::EmbeddedWindow {
                    crate::keyboard::feed(ctx, &keyboard, penumbra_ops::keyboard::Source::Viewer);
                }
                if class == egui::ViewportClass::EmbeddedWindow {
                    // Where the platform has no second window (the headless runs), egui already
                    // wraps this in a window inside the main one; it has no close button.
                    if ctx.button("close").clicked() {
                        close = true;
                    }
                    egui::Frame::NONE.fill(background).show(ctx, |ui| content(ui));
                } else {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.fill(background))
                        .show(ctx, |ui| content(ui));
                    close = ctx.input(|i| i.viewport().close_requested());
                }
            })
        };
        // Opening the window creates and configures its surface inside this call. wgpu waits
        // for an idle GPU first and fails if the cook thread submits meanwhile; a resize keeps
        // the old swapchain then, but a new surface stays unconfigured and wgpu panics on its
        // first frame. So the first frame runs with penumbra's submissions held off; later frames
        // do not, since presenting can block for a whole refresh.
        match gpu {
            Some(gpu) => gpu.alone(show),
            None => show(),
        }
        self.note_size(viewer_viewport(), size);
        if close {
            self.viewer.window = None;
        }
    }

    /// The 3D mouse: flies the viewer showing a free view or steering (the viewer's window when
    /// it has one and the main viewer does not); its left button locks and unlocks the horizon,
    /// its right one starts the free view over.
    fn fly(
        &mut self,
        ctx: &egui::Context,
        net: &penumbra_graph::Network,
        view: &ViewFrame,
        can_edit: bool,
        now: Instant,
        actions: &mut Vec<Action>,
    ) {
        let dt = now.saturating_duration_since(self.flown).as_secs_f64().min(0.05);
        self.flown = now;
        let nav = self.engine.spacenav();
        let presses = nav.take_presses();
        let motion = nav.motion();
        let session = self.engine.session();
        let types = session.types();
        let main = self
            .viewer
            .target(net, self.selected())
            .filter(|_| self.layout.sections.viewer)
            .and_then(|id| navigate::scene(net, types, id, self.viewer.output))
            .filter(|s| self.viewer.nav.mode(s) != navigate::Mode::Picture);
        let (viewer, scene, in_window) = match main {
            Some(scene) => (&mut self.viewer, scene, false),
            None => {
                let window = self
                    .viewer
                    .window
                    .and_then(|id| navigate::scene(net, types, id, self.window_viewer.output))
                    .filter(|s| self.window_viewer.nav.mode(s) != navigate::Mode::Picture);
                match window {
                    Some(scene) => (&mut self.window_viewer, scene, true),
                    None => return,
                }
            }
        };
        if presses & 1 != 0 {
            let locked = !viewer.nav.horizon_locked;
            viewer.nav.horizon_locked = locked;
            actions.push(Action::Message(
                if locked {
                    "3D mouse: horizon locked"
                } else {
                    "3D mouse: horizon unlocked, tilt the cap sideways to roll"
                }
                .into(),
            ));
        }
        if presses & 2 != 0 && viewer.nav.mode(&scene) == navigate::Mode::Free {
            viewer.nav.home(scene.node);
        }
        if !motion.moving() {
            viewer.nav.settle(now, false);
            return;
        }
        let distance = match viewer.nav.mode(&scene) {
            navigate::Mode::Free => viewer.nav.camera(scene.node).map(|o| o.distance()),
            _ => scene
                .steer
                .as_ref()
                .ok()
                .and_then(|c| navigate::stored_orbit(net, types, *c))
                .map(|o| o.distance()),
        };
        let Some(distance) = distance else { return };
        let m = Move::fly(motion.fly(), dt, distance, viewer.nav.horizon_locked);
        let doc = session.doc();
        let new_group = || doc.new_group();
        let inputs = crate::viewer::NavInputs {
            scene: &scene,
            free: free_picture(view, scene.node, viewer.output, in_window),
            net,
            types,
            can_edit,
            new_group: &new_group,
        };
        if let Err(e) = viewer.navigate(&m, &inputs, actions) {
            actions.push(Action::Message(e));
        }
        ctx.request_repaint();
    }

    /// Tells the outputs where the editor window is, so no projector opens over it.
    fn tell_outputs(&mut self, ctx: &egui::Context) {
        let rect = ctx.input(|i| {
            let v = i.viewport();
            let (r, ppp) = (v.outer_rect?, v.native_pixels_per_point?);
            let min = r.min * ppp;
            let size = r.size() * ppp;
            Some([min.x.round() as i32, min.y.round() as i32, size.x.round() as i32, size.y.round() as i32])
        });
        if rect != self.editor_rect {
            self.editor_rect = rect;
            self.engine.board().update(|s| s.editor = rect);
        }
    }

    // ------------------------------------------------------------------------- ui.* commands

    fn drain_requests(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.requests.take() else { return };
        while let Ok(req) = rx.try_recv() {
            let reply = req.reply.clone();
            match self.handle(ctx, &req) {
                Outcome::Done(r) => {
                    reply.send(r).ok();
                }
                Outcome::AfterInput => self.input_waiters.push(reply),
                Outcome::Screenshot(file) => {
                    let token = self.next_token;
                    self.next_token += 1;
                    self.shots.push(Shot {
                        token,
                        file,
                        reply,
                        settle: 3,
                        asked: false,
                        deadline: Instant::now() + SHOT_TIMEOUT,
                    });
                }
            }
        }
        self.requests = Some(rx);
    }

    /// Notes a window's size (`id`: the editor's or the viewer's). While it changes, the GPU
    /// work is held back so egui's configures succeed, and no healing happens.
    fn note_size(&mut self, id: egui::ViewportId, size: Option<egui::Vec2>) {
        let slot = usize::from(id == viewer_viewport());
        let Some(last) = self.sizes.get_mut(slot) else { return };
        if size.is_none() || *last == size {
            return;
        }
        let first = last.is_none();
        *last = size;
        if first {
            return;
        }
        let now = Instant::now();
        // The window maximizes just after it opens; the network should fill the final canvas.
        if id == egui::ViewportId::ROOT && now.duration_since(self.started) < SETTLE {
            self.editor.frame_all();
        }
        self.resized = Some((now, id));
        // Egui configures again for the new size: a failure before is answered by that one.
        self.heal_pending = false;
        if let Some(gpu) = &self.gpu {
            gpu.hold(now + RESIZE_HOLD);
        }
    }

    /// Recovers a window left black by a failed surface reconfigure (see [`Nudge`]), once its
    /// size has settled.
    fn heal_surface(&mut self, ctx: &egui::Context) {
        if let Some((id, nudge)) = self.nudge.take() {
            let command = match nudge {
                Nudge::Restore(size) => egui::ViewportCommand::InnerSize(size),
                Nudge::Remaximize => egui::ViewportCommand::Maximized(true),
            };
            ctx.send_viewport_cmd_to(id, command);
            ctx.request_repaint();
            return;
        }
        let failures = self.gpu.as_ref().map_or(0, |g| g.errors().surface_configure_failures);
        if failures > self.surface_failures {
            self.surface_failures = failures;
            self.heal_pending = true;
        }
        if !self.heal_pending {
            return;
        }
        let now = Instant::now();
        if let Some((at, _)) = self.resized
            && now.saturating_duration_since(at) < RESIZE_SETTLE
        {
            ctx.request_repaint_after(RESIZE_SETTLE);
            return;
        }
        self.heal_pending = false;
        if self.heals.1.is_some_and(|at| now.saturating_duration_since(at) > HEAL_WINDOW) {
            self.heals.0 = 0;
        }
        if self.heals.0 >= MAX_HEALS {
            if self.heals.0 == MAX_HEALS {
                log::warn!("the window surface keeps failing to reconfigure; not resizing again for a while");
                self.heals.0 += 1;
            }
            self.message = Some((
                "the window cannot be resized: the GPU is busy or its memory full (another program?)".into(),
                now,
            ));
            return;
        }
        self.heals = (self.heals.0 + 1, Some(now));
        // The window resized last; the editor's when none was.
        let id = self.resized.map_or(egui::ViewportId::ROOT, |(_, id)| id);
        let (maximized, size) = ctx.input_for(id, |i| {
            (i.viewport().maximized == Some(true), i.viewport().inner_rect.map(|r| r.size()))
        });
        log::warn!("the window surface was not reconfigured after a resize; resizing once more to recover");
        if let Some(gpu) = &self.gpu {
            gpu.hold(now + RESIZE_HOLD);
        }
        if maximized {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Maximized(false));
            self.nudge = Some((id, Nudge::Remaximize));
        } else if let Some(size) = size {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::InnerSize(size + vec2(1.0, 1.0)));
            self.nudge = Some((id, Nudge::Restore(size)));
        }
        ctx.request_repaint();
    }

    fn screenshots(&mut self, ctx: &egui::Context) {
        if self.shots.is_empty() {
            return;
        }
        let now = Instant::now();
        for shot in &mut self.shots {
            if shot.settle > 0 {
                shot.settle -= 1;
            } else if !shot.asked {
                shot.asked = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(shot.token)));
            }
        }
        let arrived: Vec<(u64, Arc<egui::ColorImage>)> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in arrived {
            if let Some(i) = self.shots.iter().position(|s| s.token == token) {
                let shot = self.shots.remove(i);
                shot.reply.send(save_png(&shot.file, &image)).ok();
            }
        }
        self.shots.retain(|s| {
            if now > s.deadline {
                s.reply.send(Err("no frame was drawn in time (window hidden?)".into())).ok();
                false
            } else {
                true
            }
        });
        ctx.request_repaint();
    }
}

/// The free view the engine drew for `node` at `output`: the main viewer's request comes
/// first, the viewer window's after it.
fn free_picture(
    view: &ViewFrame,
    node: NodeId,
    output: u32,
    window: bool,
) -> Option<&Result<penumbra_ops::freeview::Picture, String>> {
    let mut matching = view.free.iter().filter(|(r, _)| r.node == node && r.output == output).map(|(_, p)| p);
    if window { matching.next_back() } else { matching.next() }
}

/// The header of a collapsible section of the right column: a triangle and the title (both
/// toggle it, automation id `section.<id>`), then `extra` (buttons that stay usable while the
/// section is collapsed).
fn section(
    ui: &mut Ui,
    auto: &mut automation::Registry,
    id: &str,
    title: &str,
    open: &mut bool,
    extra: impl FnOnce(&mut Ui, &mut automation::Registry),
) {
    section_row(ui, false, auto, id, title, open, extra);
}

/// A section header whose `extra` widgets go on in further rows when `wrap` and the row is full.
fn section_row(
    ui: &mut Ui,
    wrap: bool,
    auto: &mut automation::Registry,
    id: &str,
    title: &str,
    open: &mut bool,
    extra: impl FnOnce(&mut Ui, &mut automation::Registry),
) {
    let row = |ui: &mut Ui| {
        let (rect, tri) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::click());
        let label = ui.add(egui::Label::new(RichText::new(title).strong()).sense(Sense::click()));
        let hovered = tri.hovered() || label.hovered();
        let color = if hovered { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
        let c = rect.center();
        let points = if *open {
            vec![c + vec2(-4.5, -2.5), c + vec2(4.5, -2.5), c + vec2(0.0, 3.5)]
        } else {
            vec![c + vec2(-2.5, -4.5), c + vec2(3.5, 0.0), c + vec2(-2.5, 4.5)]
        };
        ui.painter().add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
        auto.add(format!("section.{id}"), rect.union(label.rect), title);
        if tri.clicked() || label.clicked() {
            *open = !*open;
        }
        extra(ui, auto);
    };
    if wrap {
        ui.horizontal_wrapped(row);
    } else {
        ui.horizontal(row);
    }
}

enum Outcome {
    Done(Reply),
    /// Reply once the queued synthetic input has been processed.
    AfterInput,
    Screenshot(PathBuf),
}

fn save_png(file: &std::path::Path, image: &egui::ColorImage) -> Reply {
    let [w, h] = image.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for c in &image.pixels {
        rgba.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    let png = penumbra_gpu::encode_png_rgba8(w as u32, h as u32, rgba).map_err(|e| e.0)?;
    if let Some(dir) = file.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(file, png).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(json!({"file": file.display().to_string(), "width": w, "height": h}))
}

/// Project name, directory, app: the project is its directory.
fn window_title(dir: &std::path::Path, mode: &str) -> String {
    let name =
        dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
    format!("{name}  \u{b7}  {mode}  \u{b7}  {}  \u{b7}  penumbra", dir.display())
}

impl eframe::App for PenumbraApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Pointer events one per frame, so egui sees press, moves and release as a drag.
        let pointer = |e: &egui::Event| {
            matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::MouseWheel { .. }
            )
        };
        while let Some(e) = self.synthetic.front() {
            let is_pointer = pointer(e);
            if is_pointer && raw_input.events.iter().any(pointer) {
                break;
            }
            if let Some(e) = self.synthetic.pop_front() {
                let was_key_release = matches!(e, egui::Event::Key { pressed: false, .. });
                raw_input.events.push(e);
                if is_pointer || was_key_release {
                    break;
                }
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_requests(ctx);
        if self.synthetic.is_empty() {
            for w in self.input_waiters.drain(..) {
                w.send(Ok(json!({}))).ok();
            }
        } else {
            ctx.request_repaint();
        }
        self.screenshots(ctx);
        let size = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        self.note_size(egui::ViewportId::ROOT, size);
        self.heal_surface(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        // Before any panel consumes a key it uses itself. Not in `logic`: while the window is
        // hidden that sees the last shown frame's events again and again.
        crate::keyboard::feed(ui.ctx(), self.engine.keyboard(), penumbra_ops::keyboard::Source::Editor);
        self.frame(ui);
        self.tell_outputs(ui.ctx());
        // The native title bar follows the theme (Windows 11; elsewhere nothing happens).
        if self.title_bar != self.themed
            && let Some(claude) = self.themed
        {
            self.title_bar = Some(claude);
            let v = theme::visuals(claude);
            if !crate::titlebar::set_colors(frame, v.panel_fill, v.text_color()) {
                log::debug!("the title bar colour cannot be set on this system");
            }
        }
        // Thumbnails and the viewer change with the show clock; nothing needs 240 Hz.
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::from_gray(20).to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        self.close();
        self.engine.shutdown();
    }
}

/// The viewer's own window (`viewer.window`).
fn viewer_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("penumbra.viewer")
}

/// The region editor's inputs when `target` shows a scanned scene's regions.
fn region_inputs(
    net: &penumbra_graph::Network,
    types: &penumbra_graph::NodeTypes,
    target: Option<NodeId>,
    dir: &std::path::Path,
    can_edit: bool,
) -> Option<crate::regions::RegionInputs> {
    let node = target?;
    let scene = crate::regions::scene_of(net, types, node, dir)?;
    Some(crate::regions::RegionInputs { node, dir: dir.to_path_buf(), scene, can_edit })
}

/// What the viewer edits for node `target`: a mapper's handles, a `tex.shapes`' shapes; and the
/// names of its outputs.
fn node_editors<'a>(
    net: &'a penumbra_graph::Network,
    types: &'a penumbra_graph::NodeTypes,
    view: &'a penumbra_engine::ViewFrame,
    target: Option<NodeId>,
    can_edit: bool,
    new_group: &'a dyn Fn() -> penumbra_core::GroupId,
) -> (Option<crate::mapping::MapInputs<'a>>, Option<crate::shapes::ShapeInputs<'a>>, Vec<String>) {
    let map = target.and_then(|id| {
        Some(crate::mapping::MapInputs {
            net,
            types,
            node: id,
            mapping: view.mappings.get(&id)?,
            can_edit,
            new_group,
        })
    });
    let shapes = target
        .filter(|id| crate::shapes::is_shapes(net, *id))
        .map(|id| crate::shapes::ShapeInputs { net, types, node: id, can_edit, new_group });
    let names = target
        .and_then(|id| net.node(id))
        .and_then(|n| types.desc(&n.kind))
        .map(|d| d.outputs.iter().map(|o| o.name.clone()).collect())
        .unwrap_or_default();
    (map, shapes, names)
}
