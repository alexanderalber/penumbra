//! The project menu behind the folder button at the left end of the status bar: a new project,
//! open one, the recent ones, the demo, the shipped examples, and the project's folder in the
//! file manager.
//!
//! Opening runs on a thread of its own, since the old project's state is saved first, which can
//! take seconds; the folder dialog too. The window keeps drawing meanwhile, and the button is
//! disabled until it is done.
//!
//! Automation ids: `button.project`, then in the menu `project.new`, `project.open`,
//! `project.recent.<i>` (newest first), `project.demo`, `project.examples` (a submenu with
//! `project.example.<name>`), `project.folder`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{RichText, Ui};
use penumbra_core::Error;
use penumbra_engine::Engine;
use penumbra_engine::projects::{self, Opened};
use penumbra_project::FILE_NAME;

use crate::automation;
use crate::icons::{self, Icon};
use crate::theme;

/// What a folder from the dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Open,
    New,
}

/// Which project to open.
enum Target {
    Open(PathBuf),
    New(PathBuf),
    Demo,
    Example(String),
}

#[derive(Default)]
pub struct ProjectMenu {
    /// A folder dialog that is open.
    dialog: Option<(Pick, Receiver<Option<PathBuf>>)>,
    /// A project being opened, by name.
    opening: Option<(String, Receiver<Result<Opened, Error>>)>,
    /// The recent projects, listed when the menu opens.
    recent: Option<Vec<PathBuf>>,
    /// The shipped examples, listed when the menu opens.
    examples: Option<Vec<String>>,
}

/// The last part of `dir`, else all of it.
fn name_of(dir: &Path) -> String {
    dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl ProjectMenu {
    /// A dialog is open or a project is being opened.
    pub fn busy(&self) -> bool {
        self.dialog.is_some() || self.opening.is_some()
    }

    /// Draws the button and its menu. Returns a message for the status bar when something
    /// starts, finishes or fails.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        engine: &Arc<Engine>,
        auto: &mut automation::Registry,
    ) -> Option<String> {
        let mut message = self.poll(engine);
        let current = engine.session().dir().to_path_buf();
        let busy = self.busy();
        let r = ui.add_enabled_ui(!busy, |ui| icons::button(ui, Icon::Folder, "")).inner;
        let r = r.on_hover_text(format!("Project: {}", current.display())).on_disabled_hover_text(
            match &self.opening {
                Some((name, _)) => format!("opening {name}\u{2026}"),
                None => "a folder dialog is open".to_string(),
            },
        );
        auto.add("button.project", r.rect, "Project");
        if r.clicked() {
            self.recent = None;
            self.examples = None;
        }
        let mut chosen = None;
        egui::Popup::menu(&r).id(ui.make_persistent_id("project_menu")).show(|ui| {
            let b = ui.button("New project\u{2026}").on_hover_text("Pick or make an empty folder for it");
            auto.add("project.new", b.rect, "New project");
            if b.clicked() {
                self.pick(Pick::New, &current);
                ui.close();
            }
            let b = ui.button("Open project\u{2026}").on_hover_text(format!("A folder with a {FILE_NAME}"));
            auto.add("project.open", b.rect, "Open project");
            if b.clicked() {
                self.pick(Pick::Open, &current);
                ui.close();
            }
            ui.separator();
            let recent = self.recent.get_or_insert_with(|| engine.recent());
            if recent.is_empty() {
                ui.label(RichText::new("no recent projects").color(theme::TEXT_WEAK));
            }
            for (i, dir) in recent.iter().enumerate() {
                let open = projects::same_dir(dir, &current);
                let parent = dir.parent().map(|p| p.display().to_string()).unwrap_or_default();
                let b = ui
                    .add(
                        egui::Button::new(name_of(dir))
                            .shortcut_text(RichText::new(parent).color(theme::TEXT_WEAK))
                            .selected(open),
                    )
                    .on_hover_text(dir.display().to_string());
                auto.add(format!("project.recent.{i}"), b.rect, name_of(dir));
                if b.clicked() {
                    if !open {
                        chosen = Some(Target::Open(dir.clone()));
                    }
                    ui.close();
                }
            }
            let demo = projects::demo_dir().is_ok_and(|d| projects::same_dir(&d, &current));
            let b = ui
                .add(egui::Button::new("Demo project").selected(demo))
                .on_hover_text("The project penumbra opens when started without arguments");
            auto.add("project.demo", b.rect, "Demo project");
            if b.clicked() {
                if !demo {
                    chosen = Some(Target::Demo);
                }
                ui.close();
            }
            let examples = self.examples.get_or_insert_with(|| {
                projects::examples_source().map(|d| projects::list_examples(&d)).unwrap_or_default()
            });
            let copies = projects::examples_copy_dir().ok();
            ui.add_enabled_ui(!examples.is_empty(), |ui| {
                let r = ui.menu_button("Examples", |ui| {
                    for name in examples.iter() {
                        let copy = copies.as_ref().map(|c| c.join(name));
                        let open = copy.as_ref().is_some_and(|c| projects::same_dir(c, &current));
                        let tip = match &copy {
                            Some(c) if c.join(FILE_NAME).is_file() => {
                                format!("Opens your copy in {}, with your changes", c.display())
                            }
                            Some(c) => format!(
                                "Copied to {} and opened; the shipped one stays as it is",
                                c.display()
                            ),
                            None => "The user's data folder is unknown".to_string(),
                        };
                        let b = ui.add(egui::Button::new(name.as_str()).selected(open)).on_hover_text(tip);
                        auto.add(format!("project.example.{name}"), b.rect, name.as_str());
                        if b.clicked() {
                            if !open {
                                chosen = Some(Target::Example(name.clone()));
                            }
                            ui.close();
                        }
                    }
                });
                auto.add("project.examples", r.response.rect, "Examples");
            })
            .response
            .on_disabled_hover_text("No examples folder beside penumbra");
            ui.separator();
            let b = ui.button("Show project folder").on_hover_text(current.display().to_string());
            auto.add("project.folder", b.rect, "Show project folder");
            if b.clicked() {
                if let Err(e) = reveal(&current) {
                    message = Some(e);
                }
                ui.close();
            }
        });
        if let Some(target) = chosen {
            message = Some(self.open(engine, target));
        }
        message
    }

    /// Takes up what the dialog and the opening thread finished.
    fn poll(&mut self, engine: &Arc<Engine>) -> Option<String> {
        let mut message = None;
        if let Some((pick, rx)) = &self.dialog {
            let pick = *pick;
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.dialog = None;
                    message = Some("the folder dialog failed".to_string());
                }
                Ok(None) => self.dialog = None,
                Ok(Some(dir)) => {
                    self.dialog = None;
                    let is_project = dir.join(FILE_NAME).is_file();
                    message = Some(match (pick, is_project) {
                        (Pick::Open, false) => {
                            format!("{} holds no {FILE_NAME}; New project makes one there", dir.display())
                        }
                        // Picked for a new project, but it is one: open it rather than refuse.
                        (Pick::Open, true) | (Pick::New, true) => self.open(engine, Target::Open(dir)),
                        (Pick::New, false) => self.open(engine, Target::New(dir)),
                    });
                }
            }
        }
        if let Some((name, rx)) = &self.opening {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    message = Some(format!("opening {name} failed"));
                    self.opening = None;
                }
                Ok(Ok(opened)) => {
                    message = Some(format!("opened {}", opened.dir));
                    self.opening = None;
                }
                Ok(Err(e)) => {
                    message = Some(format!("cannot open {name}: {e}"));
                    self.opening = None;
                }
            }
        }
        message
    }

    /// Opens the folder dialog, starting beside the open project.
    fn pick(&mut self, pick: Pick, current: &Path) {
        let (tx, rx) = mpsc::channel();
        let start = current.parent().map(Path::to_path_buf);
        let title = match pick {
            Pick::Open => "Open a penumbra project",
            Pick::New => "Folder for the new project",
        };
        let spawned = std::thread::Builder::new().name("folder-dialog".into()).spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            tx.send(dialog.pick_folder()).ok();
        });
        match spawned {
            Ok(_) => self.dialog = Some((pick, rx)),
            Err(e) => log::error!("cannot open the folder dialog: {e}"),
        }
    }

    /// Starts opening `target` on a thread of its own; returns the message for the status bar.
    fn open(&mut self, engine: &Arc<Engine>, target: Target) -> String {
        let name = match &target {
            Target::Open(dir) | Target::New(dir) => name_of(dir),
            Target::Demo => "the demo project".to_string(),
            Target::Example(name) => format!("the example {name}"),
        };
        let (tx, rx) = mpsc::channel();
        let engine = engine.clone();
        let spawned = std::thread::Builder::new().name("project-open".into()).spawn(move || {
            let result = match target {
                Target::Open(dir) => projects::open(&engine, &dir),
                Target::New(dir) => projects::create(&engine, &dir),
                Target::Demo => projects::open_demo(&engine),
                Target::Example(name) => projects::open_example(&engine, &name),
            };
            tx.send(result).ok();
        });
        match spawned {
            Ok(_) => {
                let text = format!("opening {name}\u{2026}");
                self.opening = Some((name, rx));
                text
            }
            Err(e) => format!("cannot open {name}: {e}"),
        }
    }
}

/// Shows `dir` in the system's file manager (waited for on a thread, so it leaves no zombie).
fn reveal(dir: &Path) -> Result<(), String> {
    let program = if cfg!(windows) { "explorer" } else { "xdg-open" };
    let mut child = std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    std::thread::Builder::new()
        .name("file-manager".into())
        .spawn(move || {
            child.wait().ok();
        })
        .map(|_| ())
        .map_err(|e| format!("cannot wait for {program}: {e}"))
}
