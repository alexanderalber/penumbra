//! Editing the regions of a scanned scene in the viewer: a `tex.image_file` that shows a scene's
//! `regions_camera.png` (next to the `scene.json` that `scan.scene` wrote) gets this editor in
//! place of its picture. The camera picture shows every face `scan.scene` found, coloured by the
//! region it is in, dark lines between regions, faces that are off striped grey, and the faces
//! `scan.regions` suggests joining linked by a dashed line with a `+` to confirm.
//!
//! Click a face to select it, Shift or Ctrl and click to add or take away more; then **M** merges
//! the selected faces' regions into one, **S** splits the selected faces out of their regions,
//! **D** switches their regions off (or on again), **A** gives them back their default (on, unless
//! on the floor), Escape forgets the selection, Ctrl+Z and Ctrl+Y undo and redo (while the pointer
//! is over the picture). The same in the header, with a name field for the selected region.
//!
//! Every change is one `scan.regions` call with an `edit` list of camera pixels (a pixel inside
//! each face), run off the UI thread; the grouping file next to the scene is rewritten, and so
//! are the label pictures, which every `tex.image_file` showing them reloads. Undo sends the grouping
//! as it was before (`regions`), so it is a call of the same command.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::SystemTime;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use penumbra_core::NodeId;
use penumbra_engine::scan_regions::{
    self, Edit, EditorPictures, REGIONS_CAMERA, REGIONS_FILE, REGIONS_REPLY, RegionDef, RegionsArgs,
};
use penumbra_engine::scan_scene::{PLANES_CAMERA, SCENE_JSON};
use penumbra_graph::{Network, NodeTypes};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::mapping::View;
use crate::{automation, theme};

/// The node kind the editor opens on.
const IMAGE: &str = "tex.image_file";
/// Radius of a suggestion's confirm button, in points.
const CONFIRM: f32 = 8.0;
/// Most undo steps kept.
const MAX_UNDO: usize = 100;

/// What the editor needs from the app.
pub struct RegionInputs {
    pub node: NodeId,
    /// The project folder (`scene` and the scan folders in `scene.json` are relative to it).
    pub dir: PathBuf,
    /// The scene folder, as the node's `file` names it (relative or absolute).
    pub scene: PathBuf,
    pub can_edit: bool,
}

/// The scene folder whose regions `id` shows: a `tex.image_file` of a `regions_camera.png` that lies
/// next to a `scene.json`.
pub fn scene_of(net: &Network, types: &NodeTypes, id: NodeId, dir: &Path) -> Option<PathBuf> {
    let node = net.node(id)?;
    if node.kind != IMAGE {
        return None;
    }
    let file = node.param(types.desc(&node.kind)?, "file")?.as_str()?;
    let file = Path::new(file);
    if !file.file_name()?.to_str()?.eq_ignore_ascii_case(REGIONS_CAMERA) {
        return None;
    }
    let scene = file.parent()?.to_path_buf();
    dir.join(&scene).join(SCENE_JSON).is_file().then_some(scene)
}

/// The parts of `scan.regions`' reply the editor uses.
#[derive(Default, Deserialize)]
struct ReplyJson {
    #[serde(default)]
    regions: Vec<RegionJson>,
    #[serde(default)]
    disabled: Vec<RegionJson>,
    #[serde(default)]
    faces: Vec<FaceJson>,
    #[serde(default)]
    suggestions: Vec<SuggestionJson>,
    #[serde(default)]
    grouping: GroupingJson,
    #[serde(default)]
    options: Option<OptionsJson>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Default, Deserialize)]
struct RegionJson {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    faces: Vec<u16>,
    #[serde(default)]
    kind: String,
    /// Why a region is off (`given`, `floor`).
    #[serde(default)]
    by: Option<String>,
}

#[derive(Deserialize)]
struct FaceJson {
    face: u16,
    #[serde(default)]
    at: Option<[f64; 2]>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    camera_pixels: usize,
}

#[derive(Deserialize)]
struct SuggestionJson {
    faces: [u16; 2],
}

#[derive(Default, Deserialize)]
struct GroupingJson {
    #[serde(default)]
    file: Option<PathBuf>,
    #[serde(default)]
    regions: Vec<RegionDef>,
}

#[derive(Clone, Copy, Default, Deserialize)]
struct OptionsJson {
    seam: Option<u32>,
    smooth: Option<f32>,
    straight: Option<bool>,
    reach: Option<u32>,
}

/// A region as the editor draws it.
struct Group {
    faces: Vec<u16>,
    name: Option<String>,
    enabled: bool,
    /// Why it is off, or its kind.
    note: String,
    /// The smallest face number: its colour, so a region keeps it while others change.
    hue: u16,
}

#[derive(Clone, Copy)]
struct Face {
    at: [f64; 2],
    pixels: usize,
}

/// A reply, in the shape the editor needs.
#[derive(Default)]
struct Reply {
    groups: Vec<Group>,
    /// Per face number (index), its group.
    group_of: Vec<Option<usize>>,
    faces: Vec<Option<Face>>,
    face_kind: Vec<String>,
    suggestions: Vec<[u16; 2]>,
    grouping: Vec<RegionDef>,
    file: Option<PathBuf>,
    options: OptionsJson,
    message: String,
    warnings: Vec<String>,
}

impl Reply {
    fn parse(json: Json) -> Result<Reply, String> {
        let r: ReplyJson = serde_json::from_value(json).map_err(|e| format!("scan.regions' reply: {e}"))?;
        let top = r.faces.iter().map(|f| usize::from(f.face)).max().unwrap_or(0);
        let mut out = Reply {
            group_of: vec![None; top + 1],
            faces: vec![None; top + 1],
            face_kind: vec![String::new(); top + 1],
            suggestions: r.suggestions.iter().map(|s| s.faces).collect(),
            grouping: r.grouping.regions,
            file: r.grouping.file,
            options: r.options.unwrap_or_default(),
            message: r.message,
            warnings: r.warnings,
            ..Reply::default()
        };
        for f in r.faces {
            let k = usize::from(f.face);
            if let Some(at) = f.at
                && let Some(slot) = out.faces.get_mut(k)
            {
                *slot = Some(Face { at, pixels: f.camera_pixels });
            }
            if let Some(slot) = out.face_kind.get_mut(k) {
                slot.clone_from(&f.kind);
            }
        }
        let on = r.regions.into_iter().map(|g| (g, true));
        let off = r.disabled.into_iter().map(|g| (g, false));
        for (g, enabled) in on.chain(off) {
            let index = out.groups.len();
            for f in &g.faces {
                if let Some(slot) = out.group_of.get_mut(usize::from(*f)) {
                    *slot = Some(index);
                }
            }
            let note = match (&g.by, enabled) {
                (_, true) => g.kind.clone(),
                (Some(by), false) if by == "floor" => "off: floor".to_string(),
                _ => "off".to_string(),
            };
            out.groups.push(Group {
                hue: g.faces.iter().copied().min().unwrap_or(0),
                faces: g.faces,
                name: g.name,
                enabled,
                note,
            });
        }
        Ok(out)
    }

    fn group(&self, face: u16) -> Option<(usize, &Group)> {
        let k = (*self.group_of.get(usize::from(face))?)?;
        Some((k, self.groups.get(k)?))
    }

    fn at(&self, face: u16) -> Option<[f64; 2]> {
        self.faces.get(usize::from(face)).copied().flatten().map(|f| f.at)
    }

    /// The pixel to put a group's label on: inside its largest face.
    fn label_at(&self, g: &Group) -> Option<[f64; 2]> {
        g.faces
            .iter()
            .filter_map(|f| self.faces.get(usize::from(*f)).copied().flatten())
            .max_by_key(|f| f.pixels)
            .map(|f| f.at)
    }
}

/// What a finished call means for the undo stacks.
enum After {
    /// Opening: nothing to remember.
    Open,
    /// An edit: the grouping before it goes onto the undo stack.
    Edit(Vec<RegionDef>),
    /// An undo: the grouping before it goes onto the redo stack; the step it went back to, kept
    /// for when the call fails.
    Undo(Vec<RegionDef>, Vec<RegionDef>),
    Redo(Vec<RegionDef>, Vec<RegionDef>),
}

struct Done {
    pictures: Option<Arc<EditorPictures>>,
    reply: Result<Json, String>,
}

struct Job {
    rx: Receiver<Done>,
    after: After,
    label: String,
}

/// The editor: one per viewer, following the node it edits.
#[derive(Default)]
pub struct RegionEditor {
    /// The node and scene folder it shows.
    open: Option<(NodeId, PathBuf)>,
    view: View,
    pictures: Option<Arc<EditorPictures>>,
    reply: Option<Reply>,
    job: Option<Job>,
    /// Edits asked for while a call ran; they go out together when it is done.
    queued: Vec<Edit>,
    queued_label: String,
    selected: BTreeSet<u16>,
    undo: Vec<Vec<RegionDef>>,
    redo: Vec<Vec<RegionDef>>,
    overlay: Option<egui::TextureHandle>,
    photo: Option<egui::TextureHandle>,
    dirty: bool,
    /// The last call's outcome, or what went wrong.
    status: Option<(String, Color32)>,
    name: String,
    /// The region the name field was filled from (`None`: not filled since the last reply).
    name_for: Option<Option<usize>>,
    /// The picture's area in the last frame (the app leaves Ctrl+Z to the editor there).
    canvas: Option<Rect>,
}

impl RegionEditor {
    /// Whether keys pressed now belong to the editor (the pointer is over its picture).
    pub fn has_pointer(&self, ctx: &egui::Context) -> bool {
        self.open.is_some()
            && self
                .canvas
                .is_some_and(|r| ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p)))
    }

    /// Leaves the node: the next one starts fresh.
    pub fn close(&mut self) {
        if self.open.is_some() {
            *self = RegionEditor::default();
        }
    }

    fn follow(&mut self, input: &RegionInputs) {
        let key = (input.node, input.scene.clone());
        if self.open.as_ref() == Some(&key) {
            return;
        }
        *self = RegionEditor { open: Some(key), ..RegionEditor::default() };
        self.start(input, None, After::Open, "open".into());
    }

    /// Runs `scan.regions` off the UI thread: with `regions` (undo, redo), `edits`, or neither
    /// (opening: a reply on disk newer than everything it was made from is read instead).
    fn start(&mut self, input: &RegionInputs, call: Option<Call>, after: After, label: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = input.dir.clone();
        let scene = input.scene.clone();
        let need_pictures = self.pictures.is_none();
        let options = self.reply.as_ref().map(|r| (r.options, r.file.clone()));
        let spawned = std::thread::Builder::new().name("regions editor".into()).spawn(move || {
            let pictures = if need_pictures {
                match scan_regions::editor_pictures(&dir, &scene) {
                    Ok(p) => Some(Arc::new(p)),
                    Err(e) => {
                        let _ = tx.send(Done { pictures: None, reply: Err(e.0) });
                        return;
                    }
                }
            } else {
                None
            };
            let reply = run(&dir, &scene, call, options);
            let _ = tx.send(Done { pictures, reply });
        });
        match spawned {
            Ok(_) => self.job = Some(Job { rx, after, label }),
            Err(e) => self.status = Some((format!("could not start: {e}"), theme::ERROR)),
        }
    }

    /// Takes a finished call's result.
    fn poll(&mut self, input: &RegionInputs) {
        let Some(job) = &self.job else { return };
        let done = match job.rx.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Done { pictures: None, reply: Err("the call ended without a reply".into()) }
            }
        };
        let Some(job) = self.job.take() else { return };
        if let Some(p) = done.pictures {
            self.pictures = Some(p);
            self.photo = None;
        }
        match done.reply.and_then(Reply::parse) {
            Ok(reply) => {
                match job.after {
                    After::Open => {}
                    After::Edit(before) => {
                        if before != reply.grouping {
                            self.undo.push(before);
                            self.redo.clear();
                        }
                    }
                    After::Undo(before, _) => self.redo.push(before),
                    After::Redo(before, _) => self.undo.push(before),
                }
                if self.undo.len() > MAX_UNDO {
                    self.undo.remove(0);
                }
                let mut text = format!("{}: {}", job.label, reply.message);
                if let Some(w) = reply.warnings.first() {
                    text.push_str(&format!("; {w}"));
                }
                let colour = if reply.warnings.is_empty() { theme::TEXT_WEAK } else { theme::WARNING };
                self.status = Some((text, colour));
                // Faces that are gone (a rescan under the editor) leave the selection.
                self.selected.retain(|f| reply.faces.get(usize::from(*f)).is_some_and(Option::is_some));
                self.reply = Some(reply);
                self.dirty = true;
                self.name_for = None;
            }
            Err(e) => {
                // A failed undo or redo keeps its step.
                match job.after {
                    After::Undo(_, target) => self.undo.push(target),
                    After::Redo(_, target) => self.redo.push(target),
                    After::Open | After::Edit(_) => {}
                }
                self.status = Some((format!("{}: {e}", job.label), theme::ERROR));
            }
        }
        if !self.queued.is_empty() {
            let edits = std::mem::take(&mut self.queued);
            let label = std::mem::take(&mut self.queued_label);
            self.edit(input, edits, &label);
        }
    }

    /// Sends `edits` (or queues them behind the call that runs).
    fn edit(&mut self, input: &RegionInputs, edits: Vec<Edit>, label: &str) {
        if edits.is_empty() || !input.can_edit {
            return;
        }
        if self.job.is_some() {
            self.queued.extend(edits);
            if self.queued_label.is_empty() {
                self.queued_label = label.to_string();
            }
            return;
        }
        let before = self.reply.as_ref().map(|r| r.grouping.clone()).unwrap_or_default();
        self.start(input, Some(Call::Edit(edits)), After::Edit(before), label.to_string());
    }

    fn step_back(&mut self, input: &RegionInputs, redo: bool) {
        if self.job.is_some() || !input.can_edit {
            return;
        }
        let stack = if redo { &mut self.redo } else { &mut self.undo };
        let Some(target) = stack.pop() else { return };
        let now = self.reply.as_ref().map(|r| r.grouping.clone()).unwrap_or_default();
        let (after, label) = if redo {
            (After::Redo(now, target.clone()), "redo")
        } else {
            (After::Undo(now, target.clone()), "undo")
        };
        self.start(input, Some(Call::Regions(target)), after, label.into());
    }

    /// A pixel inside each selected face, to address it by.
    fn selected_points(&self) -> Vec<[f64; 2]> {
        let Some(reply) = &self.reply else { return Vec::new() };
        self.selected.iter().filter_map(|f| reply.at(*f)).collect()
    }

    /// The regions the selection touches (indices into the reply's groups).
    fn selected_groups(&self) -> BTreeSet<usize> {
        let Some(reply) = &self.reply else { return BTreeSet::new() };
        self.selected.iter().filter_map(|f| reply.group(*f).map(|(k, _)| k)).collect()
    }

    fn merge(&mut self, input: &RegionInputs) {
        let at = self.selected_points();
        if at.len() >= 2 {
            self.edit(input, vec![Edit::Merge { at, name: None }], "merge");
        }
    }

    fn split(&mut self, input: &RegionInputs) {
        let edits = self.selected_points().into_iter().map(|at| Edit::Split { at }).collect();
        self.edit(input, edits, "split");
    }

    /// Off when any selected region is on, else on.
    fn toggle(&mut self, input: &RegionInputs) {
        let any_on = self.any_selected_on();
        let edits = self
            .one_point_per_group()
            .into_iter()
            .map(|at| if any_on { Edit::Disable { at } } else { Edit::Enable { at } })
            .collect();
        self.edit(input, edits, if any_on { "off" } else { "on" });
    }

    fn auto(&mut self, input: &RegionInputs) {
        let edits = self.one_point_per_group().into_iter().map(|at| Edit::Auto { at }).collect();
        self.edit(input, edits, "default");
    }

    fn any_selected_on(&self) -> bool {
        let Some(reply) = &self.reply else { return false };
        self.selected_groups().iter().any(|k| reply.groups.get(*k).is_some_and(|g| g.enabled))
    }

    fn one_point_per_group(&self) -> Vec<[f64; 2]> {
        let Some(reply) = &self.reply else { return Vec::new() };
        let mut seen = BTreeSet::new();
        self.selected
            .iter()
            .filter(|f| reply.group(**f).is_none_or(|(k, _)| seen.insert(k)))
            .filter_map(|f| reply.at(*f))
            .collect()
    }

    /// The header: what can be done with the selection, undo and redo, then the view.
    pub fn header(&mut self, ui: &mut Ui, input: &RegionInputs, auto: &mut automation::Registry) {
        self.follow(input);
        self.poll(input);
        let editable = input.can_edit && self.reply.is_some();
        let picked = self.selected.len();
        let groups = self.selected_groups();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(editable, |ui| {
                let button = |ui: &mut Ui,
                              auto: &mut automation::Registry,
                              id: &str,
                              text: &str,
                              tip: &str,
                              on: bool| {
                    let r = ui.add_enabled(on, egui::Button::new(text)).on_hover_text(tip);
                    auto.add(format!("viewer.regions.{id}"), r.rect, text.to_string());
                    r.clicked()
                };
                if button(
                    ui,
                    auto,
                    "merge",
                    "merge",
                    "The selected faces' regions become one (M)",
                    picked >= 2,
                ) {
                    self.merge(input);
                }
                if button(
                    ui,
                    auto,
                    "split",
                    "split",
                    "The selected faces leave their regions (S)",
                    picked >= 1,
                ) {
                    self.split(input);
                }
                let any_on = self.any_selected_on();
                let (text, tip) = if any_on || picked == 0 {
                    ("off", "Switch the selected faces' regions off: nothing is projected there (D)")
                } else {
                    ("on", "Switch the selected faces' regions on again (D)")
                };
                if button(ui, auto, "toggle", text, tip, picked >= 1) {
                    self.toggle(input);
                }
                if button(
                    ui,
                    auto,
                    "default",
                    "default",
                    "On, unless the region lies on the floor (A)",
                    picked >= 1,
                ) {
                    self.auto(input);
                }
                self.name_field(ui, input, &groups, auto);
                ui.add_space(6.0);
                let undo = !self.undo.is_empty() && self.job.is_none();
                if button(
                    ui,
                    auto,
                    "undo",
                    "undo",
                    "Undo the last change of the regions (Ctrl+Z over the picture)",
                    undo,
                ) {
                    self.step_back(input, false);
                }
                let redo = !self.redo.is_empty() && self.job.is_none();
                if button(ui, auto, "redo", "redo", "Redo (Ctrl+Y over the picture)", redo) {
                    self.step_back(input, true);
                }
            });
            ui.add_space(6.0);
            self.view.controls(ui, auto);
        });
    }

    /// The name of the one region the selection is in.
    fn name_field(
        &mut self,
        ui: &mut Ui,
        input: &RegionInputs,
        groups: &BTreeSet<usize>,
        auto: &mut automation::Registry,
    ) {
        let one = (groups.len() == 1).then(|| groups.first().copied()).flatten();
        if self.name_for != Some(one) {
            self.name_for = Some(one);
            self.name =
                one.and_then(|k| self.reply.as_ref()?.groups.get(k)?.name.clone()).unwrap_or_default();
        }
        let r = ui.add_enabled(
            one.is_some(),
            egui::TextEdit::singleline(&mut self.name).desired_width(140.0).hint_text("region name"),
        );
        auto.add("viewer.regions.name", r.rect, "region name");
        let entered = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if entered
            && let Some(k) = one
            && let Some(at) = self.selected.iter().find_map(|f| {
                let reply = self.reply.as_ref()?;
                (reply.group(*f)?.0 == k).then(|| reply.at(*f)).flatten()
            })
        {
            let name = self.name.trim().to_string();
            self.edit(input, vec![Edit::Name { at, name }], "name");
        }
    }

    /// The camera picture in `area` with the faces over it.
    pub fn canvas(&mut self, ui: &mut Ui, area: Rect, input: &RegionInputs, auto: &mut automation::Registry) {
        self.follow(input);
        self.poll(input);
        self.canvas = Some(area);
        if self.job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        let painter = ui.painter_at(area);
        let response = ui.interact(area, ui.id().with("region_editor"), Sense::click_and_drag());
        self.view.steer(ui, &response, area);
        let Some(pictures) = self.pictures.clone() else {
            let text = match &self.status {
                Some((s, _)) if self.job.is_none() => s.clone(),
                _ => "reading the scene...".to_string(),
            };
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
            return;
        };
        let (w, h) = (pictures.width, pictures.height);
        let picture = self.view.picture(area, w as f32 / h.max(1) as f32);
        if self.photo.is_none()
            && let Some(photo) = &pictures.photo
        {
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], photo);
            self.photo = Some(ui.ctx().load_texture("regions photo", image, egui::TextureOptions::LINEAR));
        }
        if self.dirty || self.overlay.is_none() {
            let rgba = overlay(&pictures, self.reply.as_ref(), &self.selected);
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let options = egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                ..egui::TextureOptions::LINEAR
            };
            match &mut self.overlay {
                Some(t) => t.set(image, options),
                None => self.overlay = Some(ui.ctx().load_texture("regions overlay", image, options)),
            }
            self.dirty = false;
        }
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        match &self.photo {
            // The photo dimmed, so the colours read over it.
            Some(t) => {
                painter.image(t.id(), picture, uv, Color32::from_gray(150));
            }
            None => {
                painter.rect_filled(picture, egui::CornerRadius::ZERO, Color32::from_gray(12));
            }
        }
        if let Some(t) = &self.overlay {
            painter.image(t.id(), picture, uv, Color32::WHITE);
        }
        painter.rect_stroke(
            picture,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_white_alpha(60)),
            egui::StrokeKind::Outside,
        );
        let to_screen = |p: [f64; 2]| {
            pos2(
                picture.min.x + p[0] as f32 / w as f32 * picture.width(),
                picture.min.y + p[1] as f32 / h as f32 * picture.height(),
            )
        };
        let face_at = |p: Pos2| -> Option<u16> {
            let x = ((p.x - picture.min.x) / picture.width().max(1.0) * w as f32).floor();
            let y = ((p.y - picture.min.y) / picture.height().max(1.0) * h as f32).floor();
            if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                return None;
            }
            pictures.faces.get(y as usize * w + x as usize).copied().filter(|f| *f > 0)
        };
        let pointer = response.hover_pos();
        let hovered = pointer.and_then(face_at);

        // Labels: names, and which regions are off.
        if let Some(reply) = &self.reply {
            for g in &reply.groups {
                let text = match (&g.name, g.enabled) {
                    (Some(n), true) => n.clone(),
                    (Some(n), false) => format!("{n} (off)"),
                    (None, false) => "off".to_string(),
                    (None, true) => continue,
                };
                if let Some(at) = reply.label_at(g) {
                    label(&painter, to_screen(at), &text);
                }
            }
            for f in reply.faces.iter().enumerate().filter_map(|(k, f)| Some((k, (*f)?))) {
                let r = Rect::from_center_size(to_screen(f.1.at), vec2(8.0, 8.0));
                auto.add(format!("viewer.face.{}", f.0), r, format!("face {}", f.0));
            }
        }

        // Suggestions: a dashed line between the two faces and a + to confirm.
        let mut confirm: Option<[u16; 2]> = None;
        if let Some(reply) = &self.reply {
            for (k, pair) in reply.suggestions.iter().enumerate() {
                let (Some(a), Some(b)) = (reply.at(pair[0]), reply.at(pair[1])) else { continue };
                let (a, b) = (to_screen(a), to_screen(b));
                let stroke = Stroke::new(2.0, theme::WARNING);
                painter.extend(egui::Shape::dashed_line(&[a, b], stroke, 6.0, 4.0));
                painter.circle_filled(a, 3.0, theme::WARNING);
                painter.circle_filled(b, 3.0, theme::WARNING);
                let m = a + (b - a) * 0.5;
                let near = pointer.is_some_and(|p| p.distance(m) <= CONFIRM + 2.0);
                let fill = if near { theme::WARNING } else { Color32::from_black_alpha(200) };
                painter.circle(m, CONFIRM, fill, stroke);
                let plus = if near { Color32::BLACK } else { theme::WARNING };
                painter.line_segment([m - vec2(4.0, 0.0), m + vec2(4.0, 0.0)], Stroke::new(2.0, plus));
                painter.line_segment([m - vec2(0.0, 4.0), m + vec2(0.0, 4.0)], Stroke::new(2.0, plus));
                let r = Rect::from_center_size(m, vec2(CONFIRM * 2.0, CONFIRM * 2.0));
                auto.add(
                    format!("viewer.suggestion.{k}"),
                    r,
                    format!("join faces {} and {}", pair[0], pair[1]),
                );
                if near && response.clicked() {
                    confirm = Some(*pair);
                }
            }
        }

        let editable = input.can_edit && self.reply.is_some();
        if editable {
            if let Some(pair) = confirm {
                let at: Vec<[f64; 2]> =
                    pair.iter().filter_map(|f| self.reply.as_ref().and_then(|r| r.at(*f))).collect();
                self.edit(input, vec![Edit::Merge { at, name: None }], "join");
            } else if response.clicked() {
                // The click's own modifiers too: a synthetic click carries them only on its event.
                let add = ui.input(|i| {
                    let held = |m: egui::Modifiers| m.shift || m.command;
                    held(i.modifiers)
                        || i.events.iter().any(|e| {
                            matches!(e, egui::Event::PointerButton { pressed: false, modifiers, .. } if held(*modifiers))
                        })
                });
                match hovered {
                    Some(f) if add => {
                        if !self.selected.remove(&f) {
                            self.selected.insert(f);
                        }
                    }
                    Some(f) => self.selected = BTreeSet::from([f]),
                    None if !add => self.selected.clear(),
                    None => {}
                }
                self.dirty = true;
            }
            self.keys(ui, &response, input);
        }

        let status = self.status_line(hovered);
        if let Some((text, c)) = status {
            let galley = painter.layout_no_wrap(text, egui::FontId::monospace(12.0), c);
            let at = area.left_bottom() + vec2(6.0, -6.0 - galley.size().y);
            let back = Rect::from_min_size(at, galley.size()).expand(3.0);
            painter.rect_filled(back, egui::CornerRadius::same(3), Color32::from_black_alpha(170));
            painter.galley(at, galley, c);
        }
    }

    /// Keys while the pointer is over the picture and nothing takes text.
    fn keys(&mut self, ui: &Ui, response: &egui::Response, input: &RegionInputs) {
        if !response.hovered() || ui.memory(|m| m.focused().is_some()) {
            return;
        }
        use egui::{Key, Modifiers};
        let (merge, split, toggle, auto, clear, undo, redo) = ui.input_mut(|i| {
            let redo = i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
                || i.consume_key(Modifiers::COMMAND, Key::Y);
            (
                i.consume_key(Modifiers::NONE, Key::M),
                i.consume_key(Modifiers::NONE, Key::S),
                i.consume_key(Modifiers::NONE, Key::D),
                i.consume_key(Modifiers::NONE, Key::A),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                redo,
            )
        });
        if merge {
            self.merge(input);
        }
        if split {
            self.split(input);
        }
        if toggle {
            self.toggle(input);
        }
        if auto {
            self.auto(input);
        }
        if clear {
            self.selected.clear();
            self.dirty = true;
        }
        if undo {
            self.step_back(input, false);
        }
        if redo {
            self.step_back(input, true);
        }
    }

    fn status_line(&self, hovered: Option<u16>) -> Option<(String, Color32)> {
        if let Some(job) = &self.job {
            return Some((format!("{}: working...", job.label), theme::TEXT_WEAK));
        }
        let reply = self.reply.as_ref()?;
        let describe = |f: u16| -> String {
            let kind = reply.face_kind.get(usize::from(f)).map_or("", String::as_str);
            match reply.group(f) {
                Some((_, g)) => {
                    let region = match (&g.name, g.faces.len()) {
                        (Some(n), k) => format!("in \"{n}\" ({k} face(s))"),
                        (None, 1) => "a region of its own".to_string(),
                        (None, k) => format!("in a region of {k} faces"),
                    };
                    format!("face {f} ({kind}) {region}, {}", g.note)
                }
                None => format!("face {f} ({kind})"),
            }
        };
        if let Some(f) = hovered {
            let picked = if self.selected.is_empty() {
                String::new()
            } else {
                format!("   [{} selected]", self.selected.len())
            };
            return Some((format!("{}{picked}", describe(f)), theme::TEXT_WEAK));
        }
        if !self.selected.is_empty() {
            let text = format!(
                "{} face(s) selected: M merge, S split, D off/on, A default, Esc none",
                self.selected.len()
            );
            return Some((text, theme::TEXT_WEAK));
        }
        if let Some(s) = &self.status {
            return Some(s.clone());
        }
        Some((
            "click a face, Shift+click for more; M merge, S split, D off/on, Ctrl+Z undo".into(),
            theme::TEXT_WEAK,
        ))
    }
}

/// What a call does.
enum Call {
    Edit(Vec<Edit>),
    Regions(Vec<RegionDef>),
}

/// One `scan.regions` call (none: the reply on disk when it is newer than its sources).
fn run(
    dir: &Path,
    scene: &Path,
    call: Option<Call>,
    options: Option<(OptionsJson, Option<PathBuf>)>,
) -> Result<Json, String> {
    let scene_dir = dir.join(scene);
    let (options, file) = match options {
        Some(o) => o,
        None => saved_options(&scene_dir),
    };
    if call.is_none()
        && let Some(reply) = fresh_reply(&scene_dir, file.as_deref())
    {
        return Ok(reply);
    }
    let mut args = RegionsArgs {
        scene: Some(scene_dir),
        // The grouping the reply names when it is not the scene's own file.
        file,
        seam: options.seam,
        smooth: options.smooth,
        straight: options.straight,
        reach: options.reach,
        ..RegionsArgs::default()
    };
    match call {
        Some(Call::Edit(edits)) => args.edit = Some(edits),
        Some(Call::Regions(r)) => args.regions = Some(r),
        None => {}
    }
    scan_regions::run(dir, &args).map_err(|e| e.0)
}

/// The options and grouping file of the last reply written into the scene folder, so the editor
/// keeps what the command was last run with.
fn saved_options(scene_dir: &Path) -> (OptionsJson, Option<PathBuf>) {
    let Some(reply) = read_reply(scene_dir) else { return (OptionsJson::default(), None) };
    let options =
        reply.get("options").cloned().and_then(|o| serde_json::from_value(o).ok()).unwrap_or_default();
    let file = reply
        .get("grouping")
        .and_then(|g| g.get("file"))
        .and_then(Json::as_str)
        .map(PathBuf::from)
        .filter(|f| *f != scene_dir.join(REGIONS_FILE));
    (options, file)
}

fn read_reply(scene_dir: &Path) -> Option<Json> {
    let text = std::fs::read_to_string(scene_dir.join(REGIONS_REPLY)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The reply on disk when nothing it was made from changed since.
fn fresh_reply(scene_dir: &Path, file: Option<&Path>) -> Option<Json> {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let written = modified(&scene_dir.join(REGIONS_REPLY))?;
    let grouping = file.map_or_else(|| scene_dir.join(REGIONS_FILE), Path::to_path_buf);
    let sources = [scene_dir.join(SCENE_JSON), scene_dir.join(PLANES_CAMERA), grouping];
    let newest = sources.iter().filter_map(|p| modified(p)).max().unwrap_or(SystemTime::UNIX_EPOCH);
    // The pictures must be there too: a reply alone is no state.
    (written >= newest && scene_dir.join(REGIONS_CAMERA).is_file()).then(|| read_reply(scene_dir)).flatten()
}

/// A region's colour: hues a golden angle apart by its smallest face, so it keeps its colour
/// while other regions change.
fn hue(k: u16) -> [u8; 3] {
    palette((f32::from(k) * 0.618_034).fract())
}

/// A saturated colour at `turn` (0 to 1) around the hue circle.
fn palette(turn: f32) -> [u8; 3] {
    let h = turn.fract() * 6.0;
    let (c, v) = (0.75f32, 1.0f32);
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// Hues for the regions: each prefers one by its smallest face (so it mostly keeps its colour
/// while others change) and takes the next free one when a region next to it has that already,
/// so neighbours never look alike.
fn group_colours(pictures: &EditorPictures, reply: &Reply) -> Vec<[u8; 3]> {
    const HUES: usize = 12;
    let (w, h) = (pictures.width, pictures.height);
    let n = reply.groups.len();
    let group = |i: usize| -> Option<usize> {
        let f = pictures.faces.get(i).copied().filter(|f| *f > 0)?;
        reply.group(f).map(|(k, _)| k)
    };
    // Neighbours: regions within a few pixels of each other (seams between faces are unlabelled).
    let mut near: BTreeSet<(usize, usize)> = BTreeSet::new();
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let Some(a) = group(y * w + x) else { continue };
            for (dx, dy) in [(4usize, 0usize), (0, 4), (2, 0), (0, 2)] {
                if x + dx < w
                    && y + dy < h
                    && let Some(b) = group((y + dy) * w + x + dx)
                    && a != b
                {
                    near.insert((a.min(b), a.max(b)));
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|k| reply.groups.get(*k).map_or(0, |g| g.hue));
    let mut chosen: Vec<Option<usize>> = vec![None; n];
    for k in order {
        let first = reply.groups.get(k).map_or(0, |g| usize::from(g.hue) * 5 % HUES);
        let taken = |c: usize| {
            near.iter().any(|(a, b)| {
                let other = if *a == k {
                    *b
                } else if *b == k {
                    *a
                } else {
                    return false;
                };
                chosen.get(other).copied().flatten() == Some(c)
            })
        };
        let c = (0..HUES).map(|d| (first + d) % HUES).find(|c| !taken(*c)).unwrap_or(first);
        if let Some(slot) = chosen.get_mut(k) {
            *slot = Some(c);
        }
    }
    chosen.into_iter().map(|c| palette(c.unwrap_or(0) as f32 / HUES as f32)).collect()
}

/// The faces in their regions' colours over the photo (RGBA, not premultiplied): dark lines
/// between regions, faint ones between a region's faces, the selected faces bright with a white
/// edge, regions that are off striped grey, pixels on no face clear.
fn overlay(pictures: &EditorPictures, reply: Option<&Reply>, selected: &BTreeSet<u16>) -> Vec<u8> {
    let (w, h) = (pictures.width, pictures.height);
    let faces = &pictures.faces;
    let top = faces.iter().copied().max().map_or(0, usize::from);
    // Per face: its group (u32::MAX none), colour, alpha, whether it is on and selected.
    let mut group = vec![u32::MAX; top + 1];
    let mut colour = vec![[0u8; 4]; top + 1];
    let mut on = vec![true; top + 1];
    let mut picked = vec![false; top + 1];
    let palette = reply.map(|r| group_colours(pictures, r)).unwrap_or_default();
    for f in 1..=top {
        let Ok(face) = u16::try_from(f) else { continue };
        let g = reply.and_then(|r| r.group(face));
        let sel = selected.contains(&face);
        let (rgb, enabled) = match g {
            Some((k, g)) => (palette.get(k).copied().unwrap_or_else(|| hue(g.hue)), g.enabled),
            None => (hue(face), true),
        };
        let rgba = if sel {
            let light = rgb.map(|v| ((u16::from(v) + 255 * 2) / 3) as u8);
            [light[0], light[1], light[2], 235]
        } else {
            [rgb[0], rgb[1], rgb[2], 165]
        };
        if let (Some(gs), Some(cs), Some(os), Some(ps)) =
            (group.get_mut(f), colour.get_mut(f), on.get_mut(f), picked.get_mut(f))
        {
            *gs = g.map_or(u32::MAX - 1 - f as u32, |(k, _)| k as u32);
            *cs = rgba;
            *os = enabled;
            *ps = sel;
        }
    }
    let label = |i: usize| faces.get(i).copied().map_or(0, usize::from);
    let mut out = vec![0u8; w * h * 4];
    for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let f = label(i);
        if f == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let g = group.get(f).copied().unwrap_or(u32::MAX);
        let neighbours = [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then(|| i + w),
        ];
        let mut region_edge = false;
        let mut face_edge = false;
        for j in neighbours.into_iter().flatten() {
            let n = label(j);
            if n != f {
                face_edge = true;
                if n == 0 || group.get(n).copied() != Some(g) {
                    region_edge = true;
                }
            }
        }
        let sel = picked.get(f).copied().unwrap_or(false);
        let c = if sel && face_edge {
            [255, 255, 255, 255]
        } else if region_edge {
            [10, 10, 10, 255]
        } else if !on.get(f).copied().unwrap_or(true) {
            let light = if ((x + y) / 6).is_multiple_of(2) { 110 } else { 55 };
            let a = if sel { 245 } else { 215 };
            [light, light, light, a]
        } else if face_edge {
            let c = colour.get(f).copied().unwrap_or_default();
            [c[0] / 2, c[1] / 2, c[2] / 2, 220]
        } else {
            colour.get(f).copied().unwrap_or_default()
        };
        *px = c;
    }
    out
}

/// A name on the picture, on a dark plate.
fn label(painter: &egui::Painter, at: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
    let min = at - galley.size() * 0.5;
    let back = Rect::from_min_size(min, galley.size()).expand(2.0);
    painter.rect_filled(back, egui::CornerRadius::same(3), Color32::from_black_alpha(160));
    painter.galley(min, galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pictures() -> EditorPictures {
        // Three faces side by side, 4 x 2 pixels each.
        let mut faces = Vec::new();
        for _ in 0..2 {
            for f in [1u16, 2, 3] {
                faces.extend([f; 4]);
            }
        }
        EditorPictures { width: 12, height: 2, faces, photo: None }
    }

    fn reply() -> Reply {
        let json = serde_json::json!({
            "message": "3 face(s) in 2 region(s), 1 of them off",
            "regions": [{"region": 1, "name": "crate", "faces": [1, 2], "kind": "upright"}],
            "disabled": [{"name": null, "faces": [3], "kind": "floor", "by": "floor"}],
            "faces": [
                {"face": 1, "at": [1, 1], "kind": "upright", "camera_pixels": 8, "region": 1},
                {"face": 2, "at": [5, 1], "kind": "upright", "camera_pixels": 8, "region": 1},
                {"face": 3, "at": [9, 1], "kind": "floor", "camera_pixels": 8, "region": 0}
            ],
            "suggestions": [{"faces": [3, 2], "at": [[9, 1], [5, 1]]}],
            "grouping": {"file": "x/regions.json", "regions": [{"seeds": [[1.0, 1.0], [5.0, 1.0]], "name": "crate"}]},
            "options": {"seam": 10, "smooth": 2.5, "straight": true, "reach": 12}
        });
        Reply::parse(json).unwrap()
    }

    #[test]
    fn the_reply_groups_faces_into_regions_and_knows_which_are_off() {
        let r = reply();
        assert_eq!(r.group(1).map(|g| g.0), r.group(2).map(|g| g.0));
        assert!(r.group(1).unwrap().1.enabled);
        assert!(!r.group(3).unwrap().1.enabled);
        assert_eq!(r.group(3).unwrap().1.note, "off: floor");
        assert_eq!(r.suggestions, vec![[3, 2]]);
        assert_eq!(r.grouping.len(), 1);
        assert_eq!(r.options.seam, Some(10));
    }

    #[test]
    fn the_overlay_draws_region_borders_only_between_regions_and_stripes_what_is_off() {
        let p = pictures();
        let r = reply();
        let px = |out: &[u8], x: usize, y: usize| -> [u8; 4] {
            let i = (y * 12 + x) * 4;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        let out = overlay(&p, Some(&r), &BTreeSet::new());
        // Between faces 1 and 2 (one region): a faint face edge, not the dark region border.
        assert_ne!(px(&out, 3, 0), [10, 10, 10, 255]);
        assert_eq!(px(&out, 3, 0)[3], 220);
        // Between faces 2 and 3 (two regions): the dark border.
        assert_eq!(px(&out, 7, 0), [10, 10, 10, 255]);
        // Face 3 is off: grey.
        let off = px(&out, 9, 0);
        assert_eq!(off[0], off[1]);
        assert_eq!(off[1], off[2]);
        // A selected face's edge is white.
        let out = overlay(&p, Some(&r), &BTreeSet::from([1]));
        assert_eq!(px(&out, 3, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn merge_split_and_toggle_address_faces_by_a_pixel_inside_them() {
        let mut e = RegionEditor { reply: Some(reply()), ..RegionEditor::default() };
        e.selected = BTreeSet::from([1, 2, 3]);
        assert_eq!(e.selected_points(), vec![[1.0, 1.0], [5.0, 1.0], [9.0, 1.0]]);
        // One point per region for off and on.
        assert_eq!(e.one_point_per_group(), vec![[1.0, 1.0], [9.0, 1.0]]);
        assert!(e.any_selected_on());
        e.selected = BTreeSet::from([3]);
        assert!(!e.any_selected_on());
    }

    #[test]
    fn regions_side_by_side_never_share_a_colour() {
        let p = pictures();
        let mut r = reply();
        // Faces 1 and 2 in regions of their own now, all three side by side.
        r.groups = (1..=3u16)
            .map(|f| Group { faces: vec![f], name: None, enabled: true, note: String::new(), hue: f })
            .collect();
        r.group_of = vec![None, Some(0), Some(1), Some(2)];
        let c = group_colours(&p, &r);
        assert_ne!(c[0], c[1]);
        assert_ne!(c[1], c[2]);
    }

    #[test]
    fn hues_stay_bytes_and_differ_for_neighbouring_faces() {
        assert_ne!(hue(1), hue(2));
        assert_ne!(hue(2), hue(3));
    }
}
