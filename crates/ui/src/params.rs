//! The param panel of the selected node: one row per declared parameter (the type's, then a
//! shader's own from its header), widgets chosen by the [`ParamSpec`]. The main rows are always
//! shown; params of a named group sit in a section of their own, folded away until opened
//! (`param.group.<name>`, "more" last). Whether a section is open is remembered per node type.
//! One drag is one undo step: all edits of a drag share a history group.
//!
//! A parameter driven by a channel shows the channel's value live, its widget (or the bound
//! component's) disabled, and the source next to it. The link button binds and unbinds
//! (`param.<path>.bind`).
//!
//! The circled "i" before each name (`param.<path>.info`) opens the param's help: what it does,
//! what each choice does, default, range and unit ([`ParamSpec::help`], the same as
//! `node.kinds {kind, param}` returns). A click outside or Escape closes it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use egui::{RichText, Ui};
use penumbra_core::{Chans, GroupId, Name, NodeId, ParamKind, ParamPath, ParamSpec, Value, palette};
use penumbra_graph::{Data, DataKind, Network, NodeDef, NodeStatus, NodeTypes, Op};

use crate::icons::{self, Icon};
use crate::{Action, automation, theme};

pub struct Inputs<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    pub node: NodeId,
    pub status: Option<&'a NodeStatus>,
    /// Params the instance declares (a shader's header), when the cook thread published them.
    pub instance: Option<&'a [ParamSpec]>,
    /// For a component: the params the inner nodes its exposed params lead to declare
    /// themselves (a shader's header), by inner node.
    pub inner: &'a BTreeMap<NodeId, Vec<ParamSpec>>,
    pub can_edit: bool,
    pub project_dir: Option<&'a Path>,
    pub new_group: &'a dyn Fn() -> GroupId,
    /// Connected displays, offered for a projector's `display`.
    pub displays: &'a [penumbra_ops::out::DisplayInfo],
    /// Outputs the cook thread published (thumbnails, viewer): where the bind menu finds the
    /// channel names of chan nodes on screen.
    pub outputs: &'a BTreeMap<NodeId, Vec<Data>>,
    /// A chan node whose output wire is being dragged in the editor: dropped onto a param
    /// (or one component's field) it binds it.
    pub chan_drag: Option<NodeId>,
    /// The engine's cameras, offered for a `tex.video_in`'s `device` and `mode`.
    pub cameras: &'a penumbra_camera::Hub,
    /// The engine's audio inputs: a `audio.device_in`'s `device` names the device it reads.
    pub audio: &'a penumbra_audio::Hub,
    /// The engine's audio outputs: an `out.audio` shows how its device plays.
    pub audio_out: &'a penumbra_audio::Outputs,
    /// The engine's recordings: an `out.video` shows its own and has start and stop.
    pub record: &'a penumbra_record::Hub,
    /// The engine's scans: a `tex.scan` shows how its scan stands and has scan and cancel.
    pub scans: &'a penumbra_ops::tex::scan::Scans,
    /// The show's frame rate, when known: a special value standing for it shows it.
    pub show_rate: Option<f64>,
}

#[derive(Default)]
pub struct ParamPanel {
    /// The widget being dragged or typed into and its history group.
    group: Option<(egui::Id, GroupId)>,
    group_seen: bool,
    /// Text being typed into, by param path, committed on Enter or when focus leaves.
    buffers: HashMap<String, String>,
    rename: Option<(NodeId, String)>,
    /// Files offered for a file param, listed when its menu opens.
    files: Option<(String, Vec<String>)>,
    /// Audio inputs offered for a `audio.device_in`'s `device`, listed when its menu opens
    /// (listing takes some milliseconds, too long for every frame).
    audio_inputs: Option<(String, Result<Vec<penumbra_audio::DeviceInfo>, String>)>,
    /// Cameras offered for a `tex.video_in`'s `device` or `mode`, listed when the menu opens
    /// (listing opens every camera briefly).
    cameras: Option<(String, Result<Vec<penumbra_camera::DeviceInfo>, String>)>,
    /// Spout senders offered for a `tex.spout_in`'s `sender`, listed when the menu opens.
    spout_senders: Option<(String, Vec<penumbra_spout::Listing>)>,
    /// The faces of the scene a `tex.face_content` names, listed when its face menu opens.
    faces: Option<(String, Result<Vec<FaceOffer>, String>)>,
    /// Actions a widget asked for that are not edits of the param (a face's template).
    pending: Vec<Action>,
    /// Lines the colour row being drawn takes: swatch and fields on one, the fields below the
    /// swatch, or the fields two by two below it (narrow column).
    color_lines: u8,
    /// Lines each colour row needs, measured in its cell last frame (the cell's width is only
    /// known once the label is drawn, and the label needs the row's height).
    color_measured: HashMap<String, u8>,
    /// The label of the row being drawn when it is not the param's name (an exposed param).
    label: Option<String>,
    /// The row being drawn has a param of its own as its label (a constant's channel name).
    name_edit: Option<ParamSpec>,
    /// The row being drawn is row k of a constant and may be taken away; `removed` says it was.
    removable: Option<usize>,
    removed: bool,
    /// The project's own palettes, when they were read and from which directory: read again
    /// after a second, so an edit of the file shows without reading it every frame.
    palettes: Option<(std::time::Instant, Option<std::path::PathBuf>, Vec<palette::Palette>)>,
}

impl ParamPanel {
    /// The project's own palettes (its palettes file), read at most once a second.
    fn project_palettes(&mut self, dir: Option<&Path>) -> Vec<palette::Palette> {
        let fresh = self.palettes.as_ref().is_some_and(|(at, d, _)| {
            at.elapsed() < std::time::Duration::from_secs(1) && d.as_deref() == dir
        });
        if !fresh {
            let list = dir.map(|d| penumbra_ops::palette::load(&d.join(palette::FILE)).0).unwrap_or_default();
            self.palettes = Some((std::time::Instant::now(), dir.map(Path::to_path_buf), list));
        }
        self.palettes.as_ref().map(|(_, _, l)| l.clone()).unwrap_or_default()
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        self.group_seen = false;
        let Some(node) = input.net.node(input.node) else { return };
        let Some(path) = input.net.path_of(input.node) else { return };
        let desc = input.types.desc(&node.kind);

        // Name (rename on Enter), kind, summary, status.
        ui.horizontal(|ui| {
            let editing = self.rename.as_ref().is_some_and(|(id, _)| *id == input.node);
            if !editing {
                self.rename = Some((input.node, node.name.to_string()));
            }
            if let Some((_, text)) = self.rename.as_mut() {
                let r = ui.add_enabled(
                    input.can_edit,
                    egui::TextEdit::singleline(text).font(egui::TextStyle::Heading).desired_width(160.0),
                );
                auto.add("param.name", r.rect, "name");
                if r.lost_focus() && text.as_str() != node.name.as_str() {
                    match text.parse() {
                        // The same ops as node.rename: expose lines and node references follow.
                        Ok(name) => match penumbra_graph::rename(input.net, input.types, &path, &name) {
                            Ok(ops) => actions.push(Action::edit(ops)),
                            Err(e) => actions.push(Action::Message(format!("{e}"))),
                        },
                        Err(e) => actions.push(Action::Message(format!("{e}"))),
                    }
                    self.rename = None;
                }
            }
            ui.label(RichText::new(&node.kind).color(theme::TEXT_WEAK));
            // Not an edit, so allowed while Claude edits too.
            let r =
                ui.small_button("?").on_hover_text(format!("Documentation of {} in the browser", node.kind));
            auto.add("param.help", r.rect, format!("documentation of {}", node.kind));
            if r.clicked() {
                actions.push(Action::Help(node.kind.clone()));
            }
        });
        if let Some(d) = desc {
            ui.label(RichText::new(&d.summary).color(theme::TEXT_WEAK).small());
        }
        if let Some(s) = input.status {
            if let Some(e) = &s.error {
                ui.colored_label(theme::ERROR, e);
            }
            if let Some(w) = &s.warning {
                ui.colored_label(theme::WARNING, w);
            }
        }
        ui.horizontal(|ui| {
            let mut locked = node.locked;
            let r = ui.add_enabled(input.can_edit, egui::Checkbox::new(&mut locked, "locked"));
            auto.add("param.locked", r.rect, "locked");
            if r.changed() {
                actions.push(Action::edit(vec![Op::Lock { path: path.clone(), locked }]));
            }
            if desc.is_some_and(|d| d.stateful || d.container) {
                // Not an edit, so allowed while Claude edits too.
                let tip = if desc.is_some_and(|d| d.container) {
                    "Start over every stateful node inside this component (R over the editor)"
                } else {
                    "Start over: forget what this node holds from earlier frames (R over the editor)"
                };
                let r = ui.button("reset").on_hover_text(tip);
                auto.add("param.reset", r.rect, "reset");
                if r.clicked() {
                    actions.push(Action::Reset(vec![input.node]));
                }
            }
            if let Some(s) = input.status {
                ui.label(
                    RichText::new(format!("cook {:.2} ms", s.stats.average.as_secs_f64() * 1000.0))
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
            }
        });
        if node.kind == penumbra_ops::tex::blob_track::BLOB_TRACK {
            // Not an edit (nothing in the network changes), so allowed while Claude edits too.
            ui.horizontal(|ui| {
                let r = ui.button("learn background").on_hover_text("The next frame becomes the empty scene");
                auto.add("param.learn", r.rect, "learn background");
                if r.clicked() {
                    actions.push(Action::LearnBackground { node: input.node, delay: 0.0 });
                }
                let r =
                    ui.button("in 5 s").on_hover_text("Learns in 5 s: time to step out of the picture first");
                auto.add("param.learn_later", r.rect, "learn in 5 s");
                if r.clicked() {
                    actions.push(Action::LearnBackground { node: input.node, delay: 5.0 });
                }
            });
        }
        if node.kind == penumbra_ops::tex::VIDEO_FILE {
            // Not an edit either: a cue, allowed while Claude edits.
            let r =
                ui.button("restart").on_hover_text("Start the clip over from `start` (video_file.restart)");
            auto.add("param.restart", r.rect, "restart");
            if r.clicked() {
                actions.push(Action::RestartVideo(input.node));
            }
        }
        if node.kind == penumbra_ops::out::video::VIDEO_OUT {
            record_row(ui, input, node, auto, actions);
        }
        if node.kind == penumbra_ops::out::audio::AUDIO_OUT {
            audio_out_row(ui, input, auto);
        }
        if node.kind == penumbra_ops::tex::scan::SCAN {
            scan_row(ui, input, node, auto, actions);
        }
        if node.kind == penumbra_ops::tex::shapes::SHAPES {
            let from = self.buffers.entry(format!("{path}#trace")).or_default();
            trace_row(ui, input, node, from, auto, actions);
        }
        ui.separator();

        let Some(desc) = desc else {
            ui.colored_label(theme::ERROR, format!("unknown node kind `{}`", node.kind));
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Numbers in a monospace font: with proportional digits a changing value (a drag, a
            // bound live value) wobbled inside its field.
            ui.style_mut().drag_value_text_style = egui::TextStyle::Monospace;
            ui.add_enabled_ui(input.can_edit, |ui| {
                // A constant's params are its rows, drawn as such below.
                let constant = node.kind == penumbra_ops::chan::CONSTANT;
                let general = |_: &&ParamSpec| !constant;
                let main: Vec<&ParamSpec> =
                    desc.params.iter().filter(|s| s.group.is_empty()).filter(general).collect();
                self.rows(ui, input, node, "params", &main, auto, actions);
                if constant {
                    self.constant_rows(ui, input, node, auto, actions);
                } else {
                    self.groups(ui, input, node, "", &desc.params, auto, actions);
                }
                if desc.container {
                    self.exposed(ui, input, node, auto, actions);
                }
                if constant {
                    // Its rows are its declared params.
                } else if desc.extra_params {
                    self.instance_params(ui, input, node, auto, actions);
                } else {
                    // Values from the project file the type no longer declares.
                    let stray: Vec<(&penumbra_core::Name, &Value)> =
                        node.params.iter().filter(|(n, _)| desc.param_spec(n.as_str()).is_none()).collect();
                    let note = format!("not a param of {}", node.kind);
                    stray_values(ui, input, node, &note, &stray, auto, actions);
                }
            });
        });
        if !self.group_seen {
            self.group = None;
        }
        actions.append(&mut self.pending);
    }

    /// A component's exposed params: rows of the inner params they stand for, under the exposed
    /// names.
    fn exposed(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let mut errors = Vec::new();
        let list = input.net.exposed_with_errors(input.types, node.id, &mut errors);
        ui.add_space(6.0);
        if list.is_empty() && errors.is_empty() {
            let hint = "Nothing exposed yet: list inner params under more > expose, one per line, like `speed = lfo1.rate`.";
            ui.label(RichText::new(hint).color(theme::TEXT_WEAK).small());
            return;
        }
        ui.label(RichText::new("exposed").color(theme::TEXT_WEAK).small());
        let mut problems: Vec<String> = errors;
        egui::Grid::new("params_exposed").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for x in &list {
                let target =
                    x.target.clone().and_then(|t| input.net.resolve_alias(input.types, &t).map_err(|e| e.0));
                let found = target.as_ref().ok().and_then(|t| {
                    let id = input.net.resolve(t.node())?;
                    let inner = input.net.node(id)?;
                    let name = t.param().as_str();
                    let spec = match input.types.desc(&inner.kind)?.param_spec(name) {
                        Some(spec) => spec.clone(),
                        // A param the inner node's file declares (a shader's header).
                        None => input.inner.get(&id)?.iter().find(|s| s.name == name)?.clone(),
                    };
                    Some((inner.clone(), spec))
                });
                match (found, target) {
                    (Some((inner, spec)), _) => {
                        self.label = Some(x.name.to_string());
                        self.row(ui, input, &inner, &spec, auto, actions);
                        self.label = None;
                    }
                    (None, Ok(t)) => {
                        let declared_by_file = input
                            .net
                            .resolve(t.node())
                            .and_then(|id| input.net.node(id))
                            .and_then(|n| input.types.desc(&n.kind))
                            .is_some_and(|d| d.extra_params);
                        problems.push(if declared_by_file {
                            format!("{}: {t} is not declared by its file (yet)", x.name)
                        } else {
                            format!("{}: {t} does not exist", x.name)
                        });
                    }
                    (None, Err(e)) => problems.push(format!("{}: {e}", x.name)),
                }
            }
        });
        for p in problems {
            ui.colored_label(theme::WARNING, p);
        }
    }

    /// A `chan.constant`'s rows: the channel's name in an editable label, its value as a param
    /// row (slider, bind, reset), "-" to take the row away and "+" below to add one
    /// (`param.<path>.row.<k>.remove`, `param.<path>.row.add`).
    fn constant_rows(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        use penumbra_ops::chan::{CONSTANT_MAX_ROWS, constant_row, constant_row_specs};
        let Some(desc) = input.types.desc(&node.kind) else { return };
        let Some(path) = input.net.path_of(node.id) else { return };
        let rows = constant_rows_of(node);
        let mut remove: Option<usize> = None;
        egui::Grid::new("params_constant").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for k in 0..rows {
                let [name_spec, value_spec] = if k == 0 {
                    let (n, v) = constant_row(0);
                    match (desc.param_spec(&n), desc.param_spec(&v)) {
                        (Some(n), Some(v)) => [n.clone(), v.clone()],
                        _ => continue,
                    }
                } else {
                    constant_row_specs(k)
                };
                self.name_edit = Some(name_spec);
                self.removable = (rows > 1).then_some(k);
                self.row(ui, input, node, &value_spec, auto, actions);
                if self.removed {
                    remove = Some(k);
                }
                self.name_edit = None;
                self.removable = None;
                self.removed = false;
            }
        });
        let add = ui.add_enabled(rows < CONSTANT_MAX_ROWS, egui::Button::new("+").small());
        auto.add(format!("param.{path}.row.add"), add.rect, "add a row");
        if add.on_hover_text("add a channel").clicked() {
            actions.push(Action::edit(constant_add_ops(&path, node, rows)));
        }
        if let Some(k) = remove {
            actions.push(Action::edit(constant_remove_ops(input.net, &path, node, k, rows)));
        }
    }

    /// The label cell of a row whose label is a param of its own (a constant's channel name):
    /// a text field, committed on Enter or when focus leaves.
    fn name_field(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        spec: &ParamSpec,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) -> Option<egui::Response> {
        let path = input.net.path_of(node.id)?;
        let param = ParamPath::new(path, spec.name.parse().ok()?).ok()?;
        let key = param.to_string();
        let current =
            node.params.get(spec.name.as_str()).unwrap_or(&spec.default).as_str().unwrap_or("").to_string();
        let buffer = self.buffers.entry(key.clone()).or_insert_with(|| current.clone());
        // A fixed width: in the grid's first column the box would shrink to its text.
        let r = ui.add_sized([100.0, ui.spacing().interact_size.y], egui::TextEdit::singleline(buffer));
        auto.add(format!("param.{key}"), r.rect, spec.name.clone());
        if !r.has_focus() && !r.lost_focus() && *buffer != current {
            buffer.clone_from(&current);
        }
        if r.lost_focus() && *buffer != current {
            actions
                .push(Action::edit(vec![Op::Set { param, value: Value::Text(buffer.trim().to_string()) }]));
        }
        Some(r)
    }

    fn instance_params(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let Some(desc) = input.types.desc(&node.kind) else { return };
        ui.add_space(6.0);
        let source = if node.kind.starts_with("script.") { "from the script" } else { "from the shader" };
        ui.label(RichText::new(source).color(theme::TEXT_WEAK).small());
        let declared = input.instance.unwrap_or(&[]);
        let main: Vec<&ParamSpec> = declared.iter().filter(|s| s.group.is_empty()).collect();
        self.rows(ui, input, node, "params_instance", &main, auto, actions);
        self.groups(ui, input, node, "instance.", declared, auto, actions);
        // Values nobody declares (a renamed header param): shown so they can be removed.
        let stray: Vec<(&penumbra_core::Name, &Value)> = node
            .params
            .iter()
            .filter(|(n, _)| {
                desc.param_spec(n.as_str()).is_none() && !declared.iter().any(|s| s.name == n.as_str())
            })
            .collect();
        if input.instance.is_some() {
            stray_values(ui, input, node, "not declared by the shader", &stray, auto, actions);
        }
    }

    /// `specs` as rows of one grid.
    #[allow(clippy::too_many_arguments)]
    fn rows(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        grid: &str,
        specs: &[&ParamSpec],
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if specs.is_empty() {
            return;
        }
        egui::Grid::new(grid).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for spec in specs {
                self.row(ui, input, node, spec, auto, actions);
            }
        });
    }

    /// The named groups of `specs`, each a section folded away until opened
    /// (`param.group.<prefix><name>`). The open state belongs to the node type, so opening a
    /// render's camera section opens it for every render.
    #[allow(clippy::too_many_arguments)]
    fn groups(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        prefix: &str,
        specs: &[ParamSpec],
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        for group in penumbra_core::param::groups(specs) {
            let members: Vec<&ParamSpec> = specs.iter().filter(|s| s.group == group).collect();
            let id = format!("{prefix}{group}");
            let r = egui::CollapsingHeader::new(RichText::new(group).color(theme::TEXT_WEAK))
                .id_salt(("param_group", node.kind.as_str(), id.as_str()))
                .show(ui, |ui| {
                    self.rows(ui, input, node, &format!("params_{id}"), &members, auto, actions);
                });
            auto.add(format!("param.group.{id}"), r.header_response.rect, group);
        }
    }

    /// One parameter: label, widget, reset button.
    fn row(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        spec: &ParamSpec,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let Some(path) = input.net.path_of(node.id) else { return };
        let Ok(name) = spec.name.parse::<penumbra_core::Name>() else { return };
        let Ok(param) = ParamPath::new(path, name) else { return };
        let key = param.to_string();
        let explicit = node.params.get(spec.name.as_str());
        let stored = explicit.unwrap_or(&spec.default).clone();
        // Bound parts: `None` for the whole param, `Some(i)` for component i.
        let bound: Vec<Option<u8>> =
            node.binds.keys().filter(|k| k.param.as_str() == spec.name).map(|k| k.component).collect();
        // A bound param shows what the node cooks with.
        let live = input.status.and_then(|s| s.bound.get(spec.name.as_str()));
        let value = live.filter(|_| !bound.is_empty()).unwrap_or(&stored).clone();
        let name_edit = self.name_edit.take();
        let removable = self.removable;
        let mut remove_clicked = false;
        let mut label = RichText::new(self.label.as_deref().unwrap_or(&spec.name));
        let inactive = input.types.get(&node.kind).and_then(|t| t.inactive(node, &spec.name));
        let replaced = inactive.is_some();
        if replaced {
            label = label.color(theme::TEXT_WEAK).italics();
        } else if !bound.is_empty() {
            label = label.color(theme::kind_color(DataKind::Chan));
        } else if explicit.is_none() {
            label = label.color(theme::TEXT_WEAK);
        }
        // Too narrow for the swatch and four number fields side by side, a colour takes more
        // lines (they clipped at the right edge).
        let is_color = spec.kind == ParamKind::Color;
        self.color_lines = if is_color { self.color_measured.get(&key).copied().unwrap_or(1) } else { 1 };
        let height = row_height(ui, spec, self.color_lines);
        let one = ui.spacing().interact_size.y;
        // The grid centres a cell in its row; the label of a taller row sits at its top. The
        // help button comes first, so the buttons form a column whatever the names' lengths.
        let l = ui
            .allocate_ui_with_layout(
                egui::vec2(0.0, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    if height > one {
                        let text = ui.text_style_height(&egui::TextStyle::Body);
                        ui.add_space(((one - text) / 2.0).max(0.0));
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        help_button(ui, spec, &key, inactive, auto);
                        match &name_edit {
                            Some(name_spec) => self.name_field(ui, input, node, name_spec, auto, actions),
                            None => {
                                Some(ui.add(egui::Label::new(label).wrap_mode(egui::TextWrapMode::Extend)))
                            }
                        }
                    })
                    .inner
                },
            )
            .inner;
        if let Some(l) = l {
            if let Some(why) = inactive {
                l.on_hover_text(format!("{why}. {}", spec.full_doc()));
            } else if !spec.full_doc().is_empty() {
                l.on_hover_text(spec.full_doc());
            }
        }
        let mut new: Option<Value> = None;
        let mut active_id: Option<(egui::Id, bool)> = None;
        // Rects of the parts (slider, number fields), as `param.<path>.<i>`.
        let mut parts: Vec<egui::Rect> = Vec::new();
        // The cell spans the column's width: reset, bindings and bind button from the right
        // edge, the widget in what is left, so sliders and text fields use the whole width
        // whatever the node. A text box of several lines hangs from the top.
        let align = if height > one { egui::Align::Min } else { egui::Align::Center };
        let cell = |ui: &mut Ui| {
            if replaced {
                ui.disable();
            }
            if let Some(k) = removable {
                let r = ui.small_button("-").on_hover_text("take this row away");
                auto.add(format!("param.{}.row.{k}.remove", param.node()), r.rect, "remove the row");
                remove_clicked = r.clicked();
            }
            let reset = ui
                .add_visible(explicit.is_some(), |ui: &mut Ui| icons::small_button(ui, Icon::Reset))
                .on_hover_text("reset to the default");
            auto.add(format!("param.{key}.reset"), reset.rect, "reset");
            let binds: Vec<_> = node.binds.iter().filter(|(k, _)| k.param.as_str() == spec.name).collect();
            for (part, b) in binds.into_iter().rev() {
                let from = input.net.path_of(b.node).map_or("?".to_string(), |p| {
                    let p = level_of(input.net, node.id).write_ref(&p);
                    match &b.channel {
                        Some(c) => format!("{p}:{c}"),
                        None => p,
                    }
                });
                // The default font has no arrows; the link icon before it says enough.
                let text = match part.component {
                    Some(c) => format!("{c}: {from}"),
                    None => from,
                };
                ui.label(RichText::new(text).color(theme::kind_color(DataKind::Chan)).small());
            }
            if spec.bind_components() > 0 {
                bind_menu(ui, input, node, &param, spec, auto, actions);
            }
            if is_color {
                let lines = color_lines(ui);
                if lines != self.color_lines {
                    self.color_measured.insert(key.clone(), lines);
                    ui.ctx().request_repaint();
                }
            }
            line(ui, height, egui::Layout::left_to_right(align), |ui| {
                let mut track = |r: &egui::Response| {
                    parts.push(r.rect);
                    let active = r.dragged() || r.has_focus();
                    if r.changed() || active {
                        active_id = Some((r.id, active));
                    }
                };
                self.widget(ui, input, node, spec, &key, &value, &stored, &bound, &mut track, auto, &mut new);
            });
            reset
        };
        let row = line(ui, height, egui::Layout::right_to_left(align), cell);
        let (widget, reset) = (row.response, row.inner);
        self.removed = remove_clicked;
        auto.add(format!("param.{key}"), widget.rect, spec.name.clone());
        for (i, r) in parts.iter().enumerate() {
            auto.add(format!("param.{key}.{i}"), *r, spec.name.clone());
        }
        if let Some(src) = input.chan_drag.filter(|s| *s != node.id) {
            drop_target(ui, input, src, &param, spec, widget.rect, &parts, actions);
        }
        ui.end_row();

        let group = match active_id {
            Some((wid, active)) => {
                let g = match self.group {
                    Some((id, g)) if id == wid => g,
                    _ => {
                        let g = (input.new_group)();
                        self.group = Some((wid, g));
                        g
                    }
                };
                if active {
                    self.group_seen = true;
                }
                Some(g)
            }
            None => None,
        };
        if let Some(value) = new {
            actions.push(Action::Edit {
                ops: vec![Op::Set { param: param.clone(), value }],
                label: String::new(),
                group,
            });
        }
        if reset.clicked() {
            actions.push(Action::edit(vec![Op::Reset { param }]));
        }
    }

    /// The editing widget of one param, left to right in the width `ui` offers; ranged floats
    /// stretch their slider, text fields and menus their box to fill it.
    #[allow(clippy::too_many_arguments)]
    fn widget(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        node: &NodeDef,
        spec: &ParamSpec,
        key: &str,
        value: &Value,
        stored: &Value,
        bound: &[Option<u8>],
        track: &mut dyn FnMut(&egui::Response),
        auto: &mut automation::Registry,
        new: &mut Option<Value>,
    ) {
        let whole_bound = bound.contains(&None);
        let part_bound =
            |i: usize| whole_bound || bound.iter().any(|c| c.is_some_and(|c| usize::from(c) == i));
        match &spec.kind {
            ParamKind::Bool => {
                let mut b = value.as_bool().unwrap_or(false);
                let r = ui.add_enabled(!whole_bound, egui::Checkbox::without_text(&mut b));
                if r.changed() {
                    *new = Some(Value::Bool(b));
                }
            }
            ParamKind::Int => {
                let mut i = value.as_i64().unwrap_or(0);
                let mut dv = egui::DragValue::new(&mut i).speed(0.1);
                if let Some(r) = spec.range {
                    dv = dv.range(r.min as i64..=r.max as i64).clamp_existing_to_range(r.hard);
                }
                let dv = with_specials(dv, spec, input.show_rate);
                let r = ui.add_enabled(!whole_bound, dv);
                track(&r);
                if r.changed() {
                    *new = Some(Value::Int(i));
                }
            }
            ParamKind::Float => {
                let mut f = value.as_f64().unwrap_or(0.0);
                if spec.range.is_none() {
                    let r =
                        ui.add_enabled(!whole_bound, float_drag(&mut f, spec, whole_bound, input.show_rate));
                    track(&r);
                    if r.changed() {
                        *new = Some(Value::Float(f));
                    }
                    return;
                }
                // The number at the right end, the slider in all the width before it. Parts
                // stay in reading order: slider first. The number's width is fixed per param:
                // following its text, it moved the slider under a still pointer, which picked
                // another value, which changed the text again (the field flickered between two
                // numbers).
                let pos = spec.to_slider(f).unwrap_or(0.0);
                let number_w = number_width(ui, spec, input.show_rate);
                let one = ui.spacing().interact_size.y;
                line(ui, one, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let size = [number_w, ui.spacing().interact_size.y];
                    let number = ui
                        .add_enabled_ui(!whole_bound, |ui| {
                            ui.add_sized(size, float_drag(&mut f, spec, whole_bound, input.show_rate))
                        })
                        .inner;
                    let mut pos = pos;
                    ui.spacing_mut().slider_width = ui.available_width().max(40.0);
                    let slider = ui
                        .add_enabled(!whole_bound, egui::Slider::new(&mut pos, 0.0..=1.0).show_value(false));
                    track(&slider);
                    track(&number);
                    if number.changed() {
                        *new = Some(Value::Float(f));
                    } else if slider.changed()
                        && let Some(v) = spec.from_slider(pos)
                    {
                        *new = Some(Value::Float(v));
                    }
                });
            }
            ParamKind::Vec(n) => {
                let mut c: Vec<f64> = value.components().map(<[f64]>::to_vec).unwrap_or_default();
                c.resize(usize::from(*n), 0.0);
                let mut changed = false;
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = share(ui, c.len());
                for (i, x) in c.iter_mut().enumerate() {
                    let mut dv = egui::DragValue::new(x).speed(0.01).max_decimals(4);
                    if part_bound(i) {
                        dv = dv.fixed_decimals(3);
                    }
                    let r = ui
                        .add_enabled_ui(!part_bound(i), |ui| {
                            ui.add_sized([w, ui.spacing().interact_size.y], dv)
                        })
                        .inner;
                    track(&r);
                    changed |= r.changed();
                }
                if changed {
                    *new = vector(&keep_bound(&c, stored, &part_bound));
                }
            }
            ParamKind::Color => {
                let mut c: Vec<f64> = value.components().map(<[f64]>::to_vec).unwrap_or_default();
                c.resize(4, 1.0);
                let (r0, g0, b0, a0) = (c.first(), c.get(1), c.get(2), c.get(3));
                let get = |x: Option<&f64>| x.copied().unwrap_or(1.0) as f32;
                let mut rgba = egui::Rgba::from_rgba_unmultiplied(get(r0), get(g0), get(b0), get(a0));
                let mut changed = false;
                let swatch = |ui: &mut Ui,
                              c: &mut Vec<f64>,
                              changed: &mut bool,
                              track: &mut dyn FnMut(&egui::Response)| {
                    let r = ui
                        .add_enabled_ui(bound.is_empty(), |ui| {
                            egui::color_picker::color_edit_button_rgba(
                                ui,
                                &mut rgba,
                                egui::color_picker::Alpha::OnlyBlend,
                            )
                        })
                        .inner;
                    track(&r);
                    if r.changed() {
                        let [r1, g1, b1, a1] = rgba.to_rgba_unmultiplied();
                        *c = vec![f64::from(r1), f64::from(g1), f64::from(b1), f64::from(a1)];
                        *changed = true;
                    }
                };
                // Exact numbers, also above 1 (HDR), which the picker cannot show.
                let fields = |ui: &mut Ui,
                              c: &mut Vec<f64>,
                              parts: std::ops::Range<usize>,
                              changed: &mut bool,
                              track: &mut dyn FnMut(&egui::Response)| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let w = share(ui, parts.len());
                    for (i, x) in c.iter_mut().enumerate().filter(|(i, _)| parts.contains(i)) {
                        let mut dv = egui::DragValue::new(x).speed(0.005).max_decimals(3);
                        if part_bound(i) {
                            dv = dv.fixed_decimals(3);
                        }
                        let r = ui
                            .add_enabled_ui(!part_bound(i), |ui| {
                                ui.add_sized([w, ui.spacing().interact_size.y], dv)
                            })
                            .inner;
                        track(&r);
                        *changed |= r.changed();
                    }
                };
                let mut swatch = swatch;
                match self.color_lines {
                    1 => {
                        swatch(ui, &mut c, &mut changed, track);
                        fields(ui, &mut c, 0..4, &mut changed, track);
                    }
                    2 => {
                        ui.vertical(|ui| {
                            swatch(ui, &mut c, &mut changed, track);
                            ui.horizontal(|ui| fields(ui, &mut c, 0..4, &mut changed, track));
                        });
                    }
                    _ => {
                        ui.vertical(|ui| {
                            swatch(ui, &mut c, &mut changed, track);
                            ui.horizontal(|ui| fields(ui, &mut c, 0..2, &mut changed, track));
                            ui.horizontal(|ui| fields(ui, &mut c, 2..4, &mut changed, track));
                        });
                    }
                }
                if changed {
                    *new = vector(&keep_bound(&c, stored, &part_bound));
                }
            }
            ParamKind::Choice(choices) => {
                let current = value.as_str().unwrap_or("");
                let options: Vec<&str> = choices.iter().map(String::as_str).collect();
                choice_box(ui, key, current, &options, new);
            }
            ParamKind::Palette => {
                let project = self.project_palettes(input.project_dir);
                let resolved = palette::resolve(value, &project);
                // A channel gives a number: show the name it picks.
                let current = match (value, &resolved) {
                    (Value::Text(t), _) => t.clone(),
                    (_, Ok(Some(p))) => p.name.clone(),
                    (v, _) => v.to_string(),
                };
                let one = ui.spacing().interact_size.y;
                line(ui, one, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !whole_bound {
                        palette_menu(ui, key, &project, &resolved, auto, new);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let colours =
                            resolved.as_ref().ok().and_then(|p| p.as_ref()).map(|p| p.colours.as_slice());
                        let r = palette_strip(ui, colours, egui::vec2(64.0, one - 4.0));
                        match &resolved {
                            Err(e) => {
                                r.on_hover_text(e);
                            }
                            Ok(Some(p)) => {
                                r.on_hover_text(p.hex().join(" "));
                            }
                            Ok(None) => {
                                r.on_hover_text("no palette");
                            }
                        }
                        let buffer = self.buffers.entry(key.to_string()).or_insert_with(|| current.clone());
                        let width = ui.available_width();
                        let r = ui.add_enabled(
                            !whole_bound,
                            egui::TextEdit::singleline(buffer)
                                .desired_width(width)
                                .hint_text("a name or hex colours"),
                        );
                        track(&r);
                        if !r.has_focus() && !r.lost_focus() && *buffer != current {
                            buffer.clone_from(&current);
                        }
                        if r.lost_focus() && *buffer != current {
                            *new = Some(Value::Text(buffer.clone()));
                        }
                    });
                });
            }
            ParamKind::Text | ParamKind::File { .. } => {
                if node.kind == penumbra_ops::tex::TEXT
                    && let Some(options) = font_options(node, spec)
                {
                    let current = value.as_str().unwrap_or("");
                    let options: Vec<&str> = options.iter().map(String::as_str).collect();
                    choice_box(ui, key, current, &options, new);
                    return;
                }
                let current = value.as_str().unwrap_or("").to_string();
                // Menus at the right end, the text box in the width before them.
                let height = row_height(ui, spec, 1);
                line(ui, height, egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if let ParamKind::File { extensions } = &spec.kind {
                        self.file_menu(ui, input.project_dir, key, extensions, new);
                    }
                    if node.kind == penumbra_ops::out::PROJECTOR && spec.name == "display" {
                        display_menu(ui, input.displays, key, auto, new);
                    }
                    if node.kind == penumbra_ops::audio::AUDIO_IN && spec.name == "device" {
                        self.device_menu(ui, key, false, auto, new);
                    }
                    if node.kind == penumbra_ops::out::audio::AUDIO_OUT && spec.name == "device" {
                        self.device_menu(ui, key, true, auto, new);
                    }
                    if node.kind == penumbra_ops::tex::VIDEO_IN
                        && (spec.name == "device" || spec.name == "mode")
                    {
                        let device = input
                            .types
                            .desc(&node.kind)
                            .and_then(|d| node.param(d, "device"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        self.camera_menu(ui, input.cameras, key, spec.name == "mode", &device, auto, new);
                    }
                    if node.kind == penumbra_ops::tex::face_content::FACE_CONTENT && spec.name == "face" {
                        let scene = input
                            .types
                            .desc(&node.kind)
                            .and_then(|d| node.param(d, "scene"))
                            .and_then(Value::as_str)
                            .unwrap_or(penumbra_ops::tex::face_content::DEFAULT_SCENE)
                            .to_string();
                        self.face_menu(ui, input.project_dir, &scene, key, auto, new);
                    }
                    if node.kind == penumbra_ops::tex::SPOUT_IN && spec.name == "sender" {
                        self.spout_menu(ui, key, auto, new);
                    }
                    if node.kind == penumbra_ops::tex::NDI_IN && spec.name == "source" {
                        ndi_menu(ui, key, auto, new);
                    }
                    if node.kind == penumbra_ops::out::ndi::NDI_OUT && spec.name == "name" {
                        ndi_link(ui, key, auto);
                    }
                    if spec.node_ref {
                        node_menu(ui, input, node, key, auto, new);
                    }
                    let buffer = self.buffers.entry(key.to_string()).or_insert_with(|| current.clone());
                    let width = ui.available_width();
                    let r = if spec.multiline {
                        // A text longer than the box (a script) scrolls inside it rather than
                        // growing over the rows below.
                        egui::ScrollArea::vertical()
                            .id_salt(("multiline", key))
                            .max_height(height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(buffer)
                                        .code_editor()
                                        .desired_rows(multiline_rows(spec))
                                        .desired_width(width),
                                )
                            })
                            .inner
                    } else if node.kind == penumbra_ops::audio::AUDIO_IN && spec.name == "device" {
                        audio_device_box(ui, buffer, width, input.audio, &current)
                    } else {
                        ui.add(egui::TextEdit::singleline(buffer).desired_width(width))
                    };
                    if !r.has_focus() && !r.lost_focus() && *buffer != current {
                        // Changed elsewhere (undo, Claude): show the new value.
                        buffer.clone_from(&current);
                    }
                    if r.lost_focus() && *buffer != current {
                        *new = Some(Value::Text(buffer.clone()));
                    }
                });
            }
        }
    }

    fn file_menu(
        &mut self,
        ui: &mut Ui,
        dir: Option<&Path>,
        key: &str,
        extensions: &[String],
        new: &mut Option<Value>,
    ) {
        let Some(dir) = dir else { return };
        let r = ui.menu_button("\u{2026}", |ui| {
            if self.files.as_ref().is_none_or(|(k, _)| k != key) {
                self.files = Some((key.to_string(), list_files(dir, extensions)));
            }
            let files = self.files.as_ref().map(|(_, f)| f.as_slice()).unwrap_or(&[]);
            if files.is_empty() {
                ui.label(RichText::new("no matching files in the project directory").color(theme::TEXT_WEAK));
            }
            for f in files {
                if ui.button(f).clicked() {
                    *new = Some(Value::Text(f.clone()));
                    ui.close();
                }
            }
        });
        if r.inner.is_none() && self.files.as_ref().is_some_and(|(k, _)| k == key) {
            self.files = None;
        }
    }

    /// The audio inputs of this machine for a `audio.device_in`'s `device`, or its outputs for
    /// an `out.audio`'s (`param.<path>.device.menu`, entries `device.default` and `device.<i>`).
    fn device_menu(
        &mut self,
        ui: &mut Ui,
        key: &str,
        outputs: bool,
        auto: &mut automation::Registry,
        new: &mut Option<Value>,
    ) {
        let r = ui.menu_button("\u{2026}", |ui| {
            if self.audio_inputs.as_ref().is_none_or(|(k, _)| k != key) {
                let list = if outputs {
                    penumbra_audio::playback::output_devices()
                } else {
                    penumbra_audio::capture::devices()
                };
                self.audio_inputs = Some((key.to_string(), list));
            }
            let label = if outputs { "the system's default output" } else { "the system's default input" };
            let r = ui.button(label).on_hover_text("Leaves `device` empty");
            auto.add("device.default", r.rect, if outputs { "default output" } else { "default input" });
            if r.clicked() {
                *new = Some(Value::Text(String::new()));
                ui.close();
            }
            match self.audio_inputs.as_ref().map(|(_, l)| l) {
                Some(Ok(list)) if list.is_empty() => {
                    let text = if outputs { "no audio outputs found" } else { "no audio inputs found" };
                    ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                }
                Some(Ok(list)) => {
                    for (i, d) in list.iter().enumerate() {
                        let mut text = d.name.clone();
                        if let (Some(rate), Some(ch)) = (d.rate, d.channels) {
                            text.push_str(&format!("  {rate} Hz, {ch} ch"));
                        }
                        if d.default {
                            text.push_str("  (default)");
                        }
                        let r = ui.button(text);
                        auto.add(format!("device.{i}"), r.rect, d.name.clone());
                        if r.clicked() {
                            *new = Some(Value::Text(d.name.clone()));
                            ui.close();
                        }
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::WARNING, e);
                }
                None => {}
            }
        });
        auto.add(
            format!("param.{key}.menu"),
            r.response.rect,
            if outputs { "audio outputs" } else { "audio inputs" },
        );
        if r.inner.is_none() && self.audio_inputs.as_ref().is_some_and(|(k, _)| k == key) {
            self.audio_inputs = None;
        }
    }

    /// The faces of the scene in folder `scene` for a `tex.face_content`'s `face`
    /// (`param.<path>.menu`, entries `face.<folder>`): regions first, then the faces of
    /// `scan.scene` in a submenu. Picking one without a template makes it, off the UI thread.
    fn face_menu(
        &mut self,
        ui: &mut Ui,
        dir: Option<&Path>,
        scene: &str,
        key: &str,
        auto: &mut automation::Registry,
        new: &mut Option<Value>,
    ) {
        let Some(dir) = dir else { return };
        let mut picked: Option<FaceOffer> = None;
        let r = ui.menu_button("\u{2026}", |ui| {
            if self.faces.as_ref().is_none_or(|(k, _)| k != key) {
                self.faces = Some((key.to_string(), face_offers(dir, scene)));
            }
            match self.faces.as_ref().map(|(_, l)| l) {
                Some(Ok(list)) => {
                    let entry = |ui: &mut Ui, f: &FaceOffer, auto: &mut automation::Registry| {
                        let made = if f.made { "" } else { "  (its template is made when picked)" };
                        let r = ui.button(format!("{}  {} px{made}", f.name, f.pixels));
                        auto.add(format!("face.{}", f.folder), r.rect, f.name.clone());
                        r.clicked()
                    };
                    let (regions, faces): (Vec<&FaceOffer>, Vec<&FaceOffer>) =
                        list.iter().partition(|f| f.region);
                    if regions.is_empty() {
                        ui.label(
                            RichText::new("no regions (scan.regions groups the faces)")
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    for f in regions {
                        if entry(ui, f, auto) {
                            picked = Some(f.clone());
                            ui.close();
                        }
                    }
                    let r = ui.menu_button(format!("faces of scan.scene ({})", faces.len()), |ui| {
                        egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                            for f in &faces {
                                if entry(ui, f, auto) {
                                    picked = Some((*f).clone());
                                    ui.close();
                                }
                            }
                        });
                    });
                    auto.add("face.faces", r.response.rect, "faces of scan.scene");
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::WARNING, e);
                }
                None => {}
            }
        });
        auto.add(format!("param.{key}.menu"), r.response.rect, "faces of the scene");
        if r.inner.is_none() && self.faces.as_ref().is_some_and(|(k, _)| k == key) {
            self.faces = None;
        }
        if let Some(f) = picked {
            *new = Some(Value::Text(f.name.clone()));
            if !f.made {
                self.pending.push(Action::FaceTemplate { scene: scene.to_string(), face: f.name });
            }
        }
    }

    /// The Spout senders running now for a `tex.spout_in`'s `sender`
    /// (`param.<path>.menu`, entries `sender.default` and `sender.<i>`).
    fn spout_menu(
        &mut self,
        ui: &mut Ui,
        key: &str,
        auto: &mut automation::Registry,
        new: &mut Option<Value>,
    ) {
        let r = ui.menu_button("\u{2026}", |ui| {
            if self.spout_senders.as_ref().is_none_or(|(k, _)| k != key) {
                self.spout_senders = Some((key.to_string(), penumbra_spout::senders()));
            }
            let r = ui.button("the active sender").on_hover_text("Leaves `sender` empty");
            auto.add("sender.default", r.rect, "active sender");
            if r.clicked() {
                *new = Some(Value::Text(String::new()));
                ui.close();
            }
            let list = self.spout_senders.as_ref().map(|(_, l)| l.as_slice()).unwrap_or(&[]);
            if !penumbra_spout::SUPPORTED {
                ui.label(RichText::new("Spout exists only on Windows").color(theme::TEXT_WEAK));
            } else if list.is_empty() {
                ui.label(RichText::new("no Spout senders running").color(theme::TEXT_WEAK));
            }
            for (i, s) in list.iter().enumerate() {
                let r = ui.button(format!("{}  {}x{}, {}", s.name, s.width, s.height, s.program));
                auto.add(format!("sender.{i}"), r.rect, s.name.clone());
                if r.clicked() {
                    *new = Some(Value::Text(s.name.clone()));
                    ui.close();
                }
            }
        });
        auto.add(format!("param.{key}.menu"), r.response.rect, "Spout senders");
        if r.inner.is_none() && self.spout_senders.as_ref().is_some_and(|(k, _)| k == key) {
            self.spout_senders = None;
        }
    }

    /// The cameras of this machine for a `tex.video_in`'s `device`, or the modes of the camera
    /// its `device` picks for `mode` (`param.<path>.menu`, entries `camera.first` /
    /// `mode.best` and `camera.<i>` / `mode.<i>`).
    #[allow(clippy::too_many_arguments)]
    fn camera_menu(
        &mut self,
        ui: &mut Ui,
        hub: &penumbra_camera::Hub,
        key: &str,
        modes: bool,
        device: &str,
        auto: &mut automation::Registry,
        new: &mut Option<Value>,
    ) {
        let r = ui.menu_button("\u{2026}", |ui| {
            if self.cameras.as_ref().is_none_or(|(k, _)| k != key) {
                self.cameras = Some((key.to_string(), hub.devices()));
            }
            let list = match self.cameras.as_ref().map(|(_, l)| l) {
                Some(Ok(list)) => list,
                Some(Err(e)) => {
                    ui.colored_label(theme::WARNING, e);
                    return;
                }
                None => return,
            };
            let mut pick = |ui: &mut Ui, id: String, text: String, value: String, hover: &str| {
                let r = ui.button(text).on_hover_text(hover);
                auto.add(id, r.rect, value.clone());
                if r.clicked() {
                    *new = Some(Value::Text(value));
                    ui.close();
                }
            };
            if !modes {
                pick(
                    ui,
                    "camera.first".into(),
                    "the first camera".into(),
                    String::new(),
                    "Leaves `device` empty",
                );
                if list.is_empty() {
                    ui.label(RichText::new("no cameras found").color(theme::TEXT_WEAK));
                }
                for (i, d) in list.iter().enumerate() {
                    pick(ui, format!("camera.{i}"), d.name.clone(), d.name.clone(), &d.id);
                }
                return;
            }
            pick(
                ui,
                "mode.best".into(),
                "auto".into(),
                String::new(),
                "Leaves `mode` empty: the largest size, then the highest rate",
            );
            let Some(cam) = camera_for(list, device) else {
                ui.label(RichText::new("no camera matches `device`").color(theme::TEXT_WEAK));
                return;
            };
            if let Some(e) = &cam.error {
                ui.colored_label(theme::WARNING, e);
            }
            for (i, m) in cam.modes.iter().enumerate() {
                if m.readable {
                    pick(ui, format!("mode.{i}"), m.label.clone(), m.label.clone(), &cam.name);
                } else {
                    ui.label(RichText::new(format!("{} (not read)", m.label)).color(theme::TEXT_WEAK));
                }
            }
        });
        auto.add(
            format!("param.{key}.menu"),
            r.response.rect,
            if modes { "camera modes" } else { "cameras" },
        );
        if r.inner.is_none() && self.cameras.as_ref().is_some_and(|(k, _)| k == key) {
            self.cameras = None;
        }
    }
}

/// The camera a `device` pattern picks from a list: empty the first, else an exact name, else
/// the first containing it (as the capture thread picks).
fn camera_for<'l>(
    list: &'l [penumbra_camera::DeviceInfo],
    pattern: &str,
) -> Option<&'l penumbra_camera::DeviceInfo> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return list.first();
    }
    let lower = pattern.to_lowercase();
    list.iter()
        .find(|d| d.name.eq_ignore_ascii_case(pattern))
        .or_else(|| list.iter().find(|d| d.name.to_lowercase().contains(&lower)))
}

/// `add` in a layout as wide as `ui` offers and as high as the param's widget: one line, or the
/// box of a text of several lines. A row's grid cell centres what it is given in the row's
/// height, so a cell claiming less than it draws would sink a little lower every frame.
/// A `audio.device_in`'s `device` box. It names the device actually read: greyed inside the box
/// while `device` is empty (the system's default input), on hover otherwise. The name comes from
/// the stream already open, so nothing is listed per frame.
fn audio_device_box(
    ui: &mut Ui,
    buffer: &mut String,
    width: f32,
    hub: &penumbra_audio::Hub,
    current: &str,
) -> egui::Response {
    let device = hub.stream_status(current.trim()).and_then(|s| s.device);
    let hint = match &device {
        Some(name) => format!("default: {name}"),
        None => "the default input".to_string(),
    };
    let r = ui.add(egui::TextEdit::singleline(buffer).hint_text(hint).desired_width(width));
    match device {
        Some(name) => r.on_hover_text(format!("Reading {name}")),
        None => r.on_hover_text("No device open yet"),
    }
}

/// The circled "i" of a param row and the help it opens beside it (`param.<key>.info`, the
/// overlay `param.info`): what the param does, the meaning of each choice, then type, default,
/// range, unit and slider curve. `inactive` says why the param does not count at the moment.
fn help_button(
    ui: &mut Ui,
    spec: &ParamSpec,
    key: &str,
    inactive: Option<&str>,
    auto: &mut automation::Registry,
) {
    let popup_id = ui.make_persistent_id(("param_help", key));
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let (r, clicked) = icons::info_button(ui, open);
    auto.add(format!("param.{key}.info"), r.rect, format!("help on {}", spec.name));
    let r = if open {
        r
    } else {
        let tip = "What this param does, its range and default";
        r.on_hover_text(tip).on_disabled_hover_text(tip)
    };
    let shown = egui::Popup::from_response(&r)
        .id(popup_id)
        .open_memory(clicked.then_some(egui::SetOpenCommand::Toggle))
        // Beside the panel, over the editor, so the param's own row and its neighbours stay
        // in sight; below the button when the window has no room on the left.
        .align(egui::RectAlign::LEFT_START)
        .align_alternatives(&[egui::RectAlign::BOTTOM_START, egui::RectAlign::RIGHT_START])
        .gap(6.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| help_body(ui, &spec.help(), inactive));
    if let Some(shown) = shown {
        auto.add("param.info", shown.response.rect, format!("help on {}", spec.name));
    }
}

/// Width of the help overlay's text, in points.
const HELP_WIDTH: f32 = 300.0;

fn help_body(ui: &mut Ui, help: &penumbra_core::ParamHelp, inactive: Option<&str>) {
    ui.set_max_width(HELP_WIDTH);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&help.name).strong().color(theme::TEXT));
        let mut kind = help.type_label.clone();
        if !help.group.is_empty() {
            kind.push_str(&format!(", in {}", help.group));
        }
        ui.label(RichText::new(kind).color(theme::TEXT_WEAK).small());
    });
    ui.add_space(2.0);
    if help.doc.is_empty() && help.choices.iter().all(|c| c.doc.is_empty()) {
        ui.label(RichText::new("Not described yet.").color(theme::TEXT_WEAK).italics());
    } else if !help.doc.is_empty() {
        ui.add(egui::Label::new(&help.doc).wrap());
    }
    if let Some(why) = inactive {
        ui.add(egui::Label::new(RichText::new(format!("Not used now. {why}.")).color(theme::WARNING)).wrap());
    }
    if !help.choices.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("param_help_choices").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
            for c in &help.choices {
                ui.label(
                    RichText::new(&c.value).small().family(egui::FontFamily::Monospace).color(theme::TEXT),
                );
                ui.add(egui::Label::new(RichText::new(&c.doc).small()).wrap());
                ui.end_row();
            }
        });
    }
    if !help.specials.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("param_help_specials").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
            for line in &help.specials {
                let (value, label) = line.split_once(": ").unwrap_or((line.as_str(), ""));
                ui.label(RichText::new(value).small().family(egui::FontFamily::Monospace).color(theme::TEXT));
                ui.add(egui::Label::new(RichText::new(label).small()).wrap());
                ui.end_row();
            }
        });
    }
    ui.add_space(4.0);
    ui.separator();
    let facts = help_facts(help);
    egui::Grid::new("param_help_facts").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
        for (name, value) in facts {
            ui.label(RichText::new(name).color(theme::TEXT_WEAK).small());
            ui.add(egui::Label::new(RichText::new(value).small()).wrap());
            ui.end_row();
        }
    });
}

/// The lines below the help's text: default, range, unit, slider, channels.
fn help_facts(help: &penumbra_core::ParamHelp) -> Vec<(&'static str, String)> {
    let mut facts = Vec::new();
    let default = if help.default.is_empty() { "empty".to_string() } else { help.default.clone() };
    facts.push(("default", default));
    if !help.range.is_empty() {
        let how = if help.clamped {
            "values outside are clamped"
        } else {
            "the slider's; typed values outside are kept"
        };
        facts.push(("range", format!("{}, {how}", help.range)));
    } else if !help.unit.is_empty() {
        facts.push(("unit", help.unit.clone()));
    }
    match help.curve.as_str() {
        "log" => facts.push(("slider", "logarithmic: equal steps are equal ratios".to_string())),
        "power" => facts.push(("slider", "finer near the minimum".to_string())),
        _ => {}
    }
    if help.bindable {
        facts.push((
            "channels",
            "can drive this param: the chain-link button, or drop a chan wire on it".to_string(),
        ));
    }
    facts
}

fn line<R>(
    ui: &mut Ui,
    height: f32,
    layout: egui::Layout,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), layout, add)
}

/// Height of a param row: one line, more (a colour in a narrow column), or the box of a text of
/// several lines.
fn row_height(ui: &Ui, spec: &ParamSpec, lines: u8) -> f32 {
    let one = ui.spacing().interact_size.y;
    if spec.multiline {
        // What `TextEdit::multiline(..).code_editor().desired_rows(multiline_rows(spec))` takes.
        ui.text_style_height(&egui::TextStyle::Monospace) * multiline_rows(spec) as f32
            + 2.0 * ui.spacing().button_padding.y.max(4.0)
    } else {
        let n = f32::from(lines.max(1));
        n * one + (n - 1.0) * ui.spacing().item_spacing.y
    }
}

/// Lines a colour row needs in the width `ui` has left: 1 for swatch and four number fields
/// side by side, 2 with the fields below the swatch, 3 with them two by two. A number field is
/// never narrower than its text, so the widths are measured, not assumed (the fields still clipped
/// at a fixed guess).
fn color_lines(ui: &Ui) -> u8 {
    let s = ui.spacing();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let text = ui.fonts_mut(|f| f.layout_no_wrap("0.000".into(), font, egui::Color32::WHITE).size().x);
    let field = text + 2.0 * s.button_padding.x + 2.0;
    let gap = 4.0;
    let room = ui.available_width();
    let four = 4.0 * field + 3.0 * gap;
    if room >= s.interact_size.x + s.item_spacing.x + four {
        1
    } else if room >= four {
        2
    } else {
        3
    }
}

/// `path.name` as a param path.
fn named(path: &penumbra_core::NodePath, name: &str) -> Result<ParamPath, penumbra_core::Error> {
    path.param(name.parse()?)
}

/// How many rows a `chan.constant` has.
fn constant_rows_of(node: &NodeDef) -> usize {
    let max = penumbra_ops::chan::CONSTANT_MAX_ROWS;
    let rows = node.params.get("rows").and_then(Value::as_i64).unwrap_or(1);
    usize::try_from(rows).unwrap_or(1).clamp(1, max)
}

/// The edit that adds a row to a `chan.constant`, its channel named so that no row shares it.
fn constant_add_ops(path: &penumbra_core::NodePath, node: &NodeDef, rows: usize) -> Vec<Op> {
    use penumbra_ops::chan::{constant_row, constant_row_specs};
    let name_of = |k: usize| {
        let (name, _) = constant_row(k);
        node.params
            .get(name.as_str())
            .and_then(Value::as_str)
            .map_or_else(|| format!("chan{k}"), str::to_string)
    };
    let taken: Vec<String> = (0..rows).map(name_of).collect();
    let k = rows;
    let mut ops = Vec::new();
    let (name_param, _) = constant_row(k);
    let [name_spec, _] = constant_row_specs(k);
    let default = name_spec.default.as_str().unwrap_or("value").to_string();
    // A row left over from a lower `rows` keeps its name.
    let current =
        node.params.get(name_param.as_str()).and_then(Value::as_str).map_or(default.clone(), str::to_string);
    if taken.contains(&current) {
        let free = (k..k + 1000).map(|i| format!("chan{i}")).find(|n| !taken.contains(n)).unwrap_or(default);
        if let Ok(param) = named(path, &name_param) {
            ops.push(Op::Set { param, value: Value::Text(free) });
        }
    }
    if let Ok(param) = named(path, "rows") {
        ops.push(Op::Set { param, value: Value::Int(i64::try_from(k + 1).unwrap_or(1)) });
    }
    ops
}

/// The edit that takes row `k` (from 0) of `rows` away from a `chan.constant`: the rows after it move up
/// one (names, values and bindings), the last one is cleared. One undo step.
fn constant_remove_ops(
    net: &Network,
    path: &penumbra_core::NodePath,
    node: &NodeDef,
    k: usize,
    rows: usize,
) -> Vec<Op> {
    use penumbra_ops::chan::{constant_row, constant_row_specs};
    let mut ops = Vec::new();
    let default_name = |j: usize| constant_row_specs(j)[0].default.as_str().unwrap_or("value").to_string();
    let bind_of =
        |param: &str| node.binds.iter().find(|(p, _)| p.param.as_str() == param && p.component.is_none());
    for j in k..rows {
        let (name_j, value_j) = constant_row(j);
        let (Ok(name_p), Ok(value_p)) = (named(path, &name_j), named(path, &value_j)) else { continue };
        if bind_of(&value_j).is_some() {
            ops.push(Op::Unbind { param: value_p.clone(), component: None });
        }
        if j + 1 == rows {
            ops.push(Op::Reset { param: name_p });
            ops.push(Op::Reset { param: value_p });
            continue;
        }
        // Row j takes row j + 1's name (also a default one, so the channel keeps its name),
        // value and binding.
        let (name_next, value_next) = constant_row(j + 1);
        let name = node
            .params
            .get(name_next.as_str())
            .and_then(Value::as_str)
            .map_or_else(|| default_name(j + 1), str::to_string);
        if name == default_name(j) {
            ops.push(Op::Reset { param: name_p });
        } else {
            ops.push(Op::Set { param: name_p, value: Value::Text(name) });
        }
        match node.params.get(value_next.as_str()) {
            Some(v) => ops.push(Op::Set { param: value_p.clone(), value: v.clone() }),
            None => ops.push(Op::Reset { param: value_p.clone() }),
        }
        if let Some((_, b)) = bind_of(&value_next)
            && let Some(from) = net.path_of(b.node)
        {
            ops.push(Op::Bind { param: value_p, component: None, from, channel: b.channel.clone() });
        }
    }
    if let Ok(param) = named(path, "rows") {
        ops.push(Op::Set {
            param,
            value: Value::Int(i64::try_from(rows.saturating_sub(1).max(1)).unwrap_or(1)),
        });
    }
    ops
}

/// Lines of a multiline text param's box, unless the param asks for more ([`ParamSpec::lines`]).
const MULTILINE_ROWS: usize = 6;

fn multiline_rows(spec: &ParamSpec) -> usize {
    if spec.lines == 0 { MULTILINE_ROWS } else { usize::from(spec.lines) }
}

/// The width of each of `n` fields sharing what is left of the line.
fn share(ui: &Ui, n: usize) -> f32 {
    let n = n.max(1) as f32;
    let gaps = ui.spacing().item_spacing.x * (n - 1.0);
    ((ui.available_width() - gaps) / n).floor().max(24.0)
}

/// A width for a ranged float's number field that holds its usual values: the widest of the
/// range's ends with four decimals and the unit, and of its special values' labels.
fn number_width(ui: &Ui, spec: &ParamSpec, show_rate: Option<f64>) -> f32 {
    let unit = spec.unit.symbol();
    let unit = if unit.is_empty() { String::new() } else { format!(" {unit}") };
    let (lo, hi) = spec.range.map_or((0.0, 1.0), |r| (r.min, r.max));
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let width =
        |text: String| ui.fonts_mut(|f| f.layout_no_wrap(text, font.clone(), egui::Color32::WHITE).size().x);
    let numbers = [lo, hi].iter().map(|v| width(format!("{v:.3}{unit}"))).fold(0.0, f32::max);
    let labels = spec.specials.iter().map(|s| width(special_text(s, show_rate))).fold(0.0, f32::max);
    let max = if spec.specials.is_empty() { 110.0 } else { 170.0 };
    (numbers.max(labels) + 2.0 * ui.spacing().button_padding.x).clamp(48.0, max)
}

/// What a number field shows for special value `s`: its label, and in brackets what it comes
/// out as when that is known (`show rate (60 Hz)`).
fn special_text(s: &penumbra_core::Special, show_rate: Option<f64>) -> String {
    match (s.resolves, show_rate) {
        (Some(penumbra_core::Resolves::ShowRate), Some(rate)) => {
            let rate = egui::emath::format_with_decimals_in_range(rate, 0..=2);
            format!("{} ({rate} Hz)", s.label)
        }
        _ => s.label.clone(),
    }
}

/// `dv` showing `spec`'s special values by name (typing the name sets them, numbers still
/// work) and its unit after any other value. Without special values only the unit.
fn with_specials<'a>(
    dv: egui::DragValue<'a>,
    spec: &ParamSpec,
    show_rate: Option<f64>,
) -> egui::DragValue<'a> {
    let unit = spec.unit.symbol();
    if spec.specials.is_empty() {
        return if unit.is_empty() { dv } else { dv.suffix(format!(" {unit}")) };
    }
    let shown: Vec<(f64, String)> =
        spec.specials.iter().map(|s| (s.value, special_text(s, show_rate))).collect();
    let unit = if unit.is_empty() { String::new() } else { format!(" {unit}") };
    let named = spec.clone();
    let symbol = spec.unit.symbol().to_string();
    dv.custom_formatter(move |n, decimals| match shown.iter().find(|(v, _)| *v == n) {
        Some((_, text)) => text.clone(),
        None => format!("{}{unit}", egui::emath::format_with_decimals_in_range(n, decimals)),
    })
    .custom_parser(move |text| {
        if let Some(s) = named.special_named(text) {
            return Some(s.value);
        }
        let text = text.trim();
        let text = if symbol.is_empty() { text } else { text.strip_suffix(symbol.as_str()).unwrap_or(text) };
        let digits: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '\u{2212}' { '-' } else { c })
            .collect();
        digits.parse().ok()
    })
}

/// The number field of a float param.
fn float_drag<'a>(
    f: &'a mut f64,
    spec: &ParamSpec,
    live: bool,
    show_rate: Option<f64>,
) -> egui::DragValue<'a> {
    let speed = spec.range.map_or(0.01, |r| (r.max - r.min) / 300.0);
    let mut dv = egui::DragValue::new(f).speed(speed).max_decimals(4);
    if live {
        // A live value with a fixed number of decimals, so the field does not change width
        // every frame.
        dv = dv.fixed_decimals(3);
    }
    if let Some(r) = spec.range.filter(|r| r.hard) {
        dv = dv.range(r.min..=r.max);
    }
    with_specials(dv, spec, show_rate)
}

/// A menu of `options` as wide as `ui` offers. A value not among them (a font that is not
/// installed here) stays shown as the current one.
/// A strip of `colours` side by side in `size` (a dashed outline for none).
fn palette_strip(ui: &mut Ui, colours: Option<&[[f64; 4]]>, size: egui::Vec2) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    match colours.filter(|c| !c.is_empty()) {
        Some(colours) => {
            let w = rect.width() / colours.len() as f32;
            for (i, c) in colours.iter().enumerate() {
                let [cr, cg, cb, ca] = c.map(|x| x as f32);
                let fill = egui::Color32::from(egui::Rgba::from_rgba_unmultiplied(cr, cg, cb, ca));
                let x = rect.left() + w * i as f32;
                let cell =
                    egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + w, rect.bottom()));
                painter.rect_filled(cell, 0.0, fill);
            }
            painter.rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0, theme::TEXT_WEAK),
                egui::StrokeKind::Inside,
            );
        }
        None => {
            painter.rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0, theme::TEXT_WEAK),
                egui::StrokeKind::Inside,
            );
        }
    }
    r
}

/// The palettes to pick from for a palette param (`param.<path>.menu`, entries
/// `palette.<name>` and `palette.none`): the built-in palettes, colormaps and the project's
/// own, each with its colours.
fn palette_menu(
    ui: &mut Ui,
    key: &str,
    project: &[palette::Palette],
    current: &Result<Option<palette::Palette>, String>,
    auto: &mut automation::Registry,
    new: &mut Option<Value>,
) {
    let picked = current.as_ref().ok().and_then(|p| p.as_ref()).map(|p| p.name.as_str());
    let r = ui.menu_button("\u{2026}", |ui| {
        let all = palette::all(project);
        let sections = [
            ("palettes", palette::Source::Builtin, palette::Kind::Palette),
            ("colormaps", palette::Source::Builtin, palette::Kind::Colormap),
            ("this project (palettes.txt)", palette::Source::Project, palette::Kind::Palette),
        ];
        egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
            for (title, source, kind) in sections {
                let list: Vec<&palette::Palette> = all
                    .iter()
                    .filter(|p| p.source == source && (source == palette::Source::Project || p.kind == kind))
                    .collect();
                if list.is_empty() {
                    continue;
                }
                ui.label(RichText::new(title).color(theme::TEXT_WEAK).small());
                for p in list {
                    let r = ui
                        .horizontal(|ui| {
                            palette_strip(ui, Some(&p.colours), egui::vec2(72.0, 14.0));
                            ui.selectable_label(picked == Some(p.name.as_str()), &p.name)
                        })
                        .inner;
                    auto.add(format!("palette.{}", p.name), r.rect, p.name.clone());
                    if r.clicked() {
                        *new = Some(Value::Text(p.name.clone()));
                        ui.close();
                    }
                }
            }
            ui.separator();
            let r = ui.button("none").on_hover_text("Leaves the palette empty");
            auto.add("palette.none", r.rect, "no palette");
            if r.clicked() {
                *new = Some(Value::Text(String::new()));
                ui.close();
            }
        });
    });
    auto.add(format!("param.{key}.menu"), r.response.rect, "palettes");
}

fn choice_box(ui: &mut Ui, key: &str, current: &str, options: &[&str], new: &mut Option<Value>) {
    egui::ComboBox::from_id_salt(key)
        .selected_text(current)
        .width(ui.available_width())
        .height(400.0)
        .show_ui(ui, |ui| {
            for &choice in options {
                if ui.selectable_label(choice == current, choice).clicked() && choice != current {
                    *new = Some(Value::Text(choice.to_string()));
                }
            }
        });
}

/// What `tex.text`'s `font` and `style` offer: the installed and bundled families, and the
/// styles of the node's family. `None` (a plain text box) while the fonts are being scanned,
/// for any other param, and for the style of a family that is not installed.
fn font_options(node: &NodeDef, spec: &ParamSpec) -> Option<Vec<String>> {
    let families = penumbra_ops::tex::font_families()?;
    match spec.name.as_str() {
        "font" => Some(families.iter().map(|(f, _)| f.clone()).collect()),
        "style" => {
            let family =
                node.params.get("font").and_then(Value::as_str).unwrap_or(penumbra_ops::tex::DEFAULT_FONT);
            families.iter().find(|(f, _)| f.eq_ignore_ascii_case(family)).map(|(_, styles)| styles.clone())
        }
        _ => None,
    }
}

/// A face a `tex.face_content` can show, as its face menu offers it.
#[derive(Clone, Debug)]
struct FaceOffer {
    name: String,
    folder: String,
    pixels: u64,
    region: bool,
    /// Its template exists.
    made: bool,
}

/// The faces of the scene in `scene` (relative to the project `dir`), as `scan.faces` lists them.
fn face_offers(dir: &Path, scene: &str) -> Result<Vec<FaceOffer>, String> {
    use penumbra_engine::scan_faces;
    use penumbra_ops::tex::face_content::{FACES_FOLDER, UV_CAMERA};
    let scene_dir = scan_faces::scene_dir(dir, Some(Path::new(scene)));
    let (list, _) = scan_faces::surfaces(&scene_dir).map_err(|e| e.0)?;
    Ok(list
        .into_iter()
        .map(|s| FaceOffer {
            made: scene_dir.join(FACES_FOLDER).join(&s.folder).join(UV_CAMERA).exists(),
            name: s.name,
            folder: s.folder,
            pixels: s.camera_pixels,
            region: s.kind == scan_faces::Kind::Region,
        })
        .collect())
}

/// The connected displays for a projector's `display` (`param.<path>.display.menu`, entries
/// `display.<i>`).
fn display_menu(
    ui: &mut Ui,
    displays: &[penumbra_ops::out::DisplayInfo],
    key: &str,
    auto: &mut automation::Registry,
    new: &mut Option<Value>,
) {
    let r = ui.menu_button("\u{2026}", |ui| {
        if displays.is_empty() {
            ui.label(RichText::new("no displays known (headless?)").color(theme::TEXT_WEAK));
        }
        for (i, d) in displays.iter().enumerate() {
            let hz = d.refresh_hz.map_or(String::new(), |hz| format!(" @ {hz:.0} Hz"));
            let mut text = RichText::new(format!("{}  {}x{}{hz}", d.id, d.size[0], d.size[1]));
            if d.editor {
                text = text.color(theme::TEXT_WEAK);
            }
            let mut r = ui.button(text);
            if d.editor {
                r = r.on_hover_text("This display shows the editor; a projector on it stays closed");
            }
            auto.add(format!("display.{i}"), r.rect, d.id.clone());
            if r.clicked() {
                *new = Some(Value::Text(d.id.clone()));
                ui.close();
            }
        }
    });
    auto.add(format!("param.{key}.menu"), r.response.rect, "displays");
}

/// The nodes a node reference param can name: those with an output of the node's own output
/// kind, other than itself (`param.<path>.<name>.menu`, entries `noderef.<i>`).
fn node_menu(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    key: &str,
    auto: &mut automation::Registry,
    new: &mut Option<Value>,
) {
    let kind = input.types.desc(&node.kind).and_then(|d| d.outputs.first()).map(|o| o.kind);
    let level = level_of(input.net, node.id);
    let r = ui.menu_button("\u{2026}", |ui| {
        let mut paths: Vec<String> = input
            .net
            .nodes()
            .filter(|n| n.id != node.id)
            .filter(|n| {
                let outs = input.types.desc(&n.kind).map(|d| d.outputs.as_slice()).unwrap_or(&[]);
                outs.iter().any(|o| Some(o.kind) == kind)
            })
            .filter_map(|n| input.net.path_of(n.id).map(|p| level.write_ref(&p)))
            .collect();
        sort_refs(&mut paths);
        if paths.is_empty() {
            ui.label(RichText::new("no node to name").color(theme::TEXT_WEAK));
        }
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for (i, p) in paths.iter().enumerate() {
                let r = ui.button(p);
                auto.add(format!("noderef.{i}"), r.rect, p.clone());
                if r.clicked() {
                    *new = Some(Value::Text(p.clone()));
                    ui.close();
                }
            }
        });
    });
    auto.add(format!("param.{key}.menu"), r.response.rect, "nodes");
}

/// Values nobody declares, each with a remove button (`param.<path>.<name>.remove`).
fn stray_values(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    note: &str,
    stray: &[(&penumbra_core::Name, &Value)],
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    if stray.is_empty() {
        return;
    }
    let Some(path) = input.net.path_of(node.id) else { return };
    ui.add_space(6.0);
    ui.label(RichText::new(note).color(theme::WARNING).small());
    for (name, value) in stray {
        ui.horizontal(|ui| {
            ui.label(format!("{name} = {value}"));
            let r = icons::small_button(ui, Icon::Remove).on_hover_text("remove");
            auto.add(format!("param.{path}.{name}.remove"), r.rect, "remove");
            if r.clicked()
                && let Ok(param) = ParamPath::new(path.clone(), (*name).clone())
            {
                actions.push(Action::edit(vec![Op::Reset { param }]));
            }
        });
    }
}

/// `edited` with every bound component taken from `stored` instead: the widgets show the
/// channel's values, but an edit of another component must not write them into the project.
fn keep_bound(edited: &[f64], stored: &Value, bound: &dyn Fn(usize) -> bool) -> Vec<f64> {
    let stored = stored.components().unwrap_or(&[]);
    edited
        .iter()
        .enumerate()
        .map(|(i, x)| if bound(i) { stored.get(i).copied().unwrap_or(*x) } else { *x })
        .collect()
}

/// While a chan wire is dragged: outlines the param (or the vector component) under the
/// pointer and binds it on release. Scalars take the whole row, vectors and colours the field of
/// one component.
#[allow(clippy::too_many_arguments)]
fn drop_target(
    ui: &mut Ui,
    input: &Inputs<'_>,
    src: NodeId,
    param: &ParamPath,
    spec: &ParamSpec,
    row: egui::Rect,
    parts: &[egui::Rect],
    actions: &mut Vec<Action>,
) {
    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else { return };
    let target = match (&spec.kind, spec.bind_components()) {
        (_, 0) => None,
        (_, 1) => row.contains(pointer).then_some((row, None)),
        // The colour's first part is the picker; its numbers follow.
        (ParamKind::Color, _) => parts
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, r)| r.contains(pointer))
            .and_then(|(i, r)| Some((*r, Some(u8::try_from(i - 1).ok()?)))),
        _ => parts
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(pointer))
            .and_then(|(i, r)| Some((*r, Some(u8::try_from(i).ok()?)))),
    };
    let Some((rect, component)) = target else { return };
    ui.painter().rect_stroke(
        rect.expand(2.0),
        egui::CornerRadius::same(3),
        egui::Stroke::new(2.0, theme::kind_color(DataKind::Chan)),
        egui::StrokeKind::Outside,
    );
    if ui.input(|i| i.pointer.primary_released())
        && input.can_edit
        && let Some(from) = input.net.path_of(src)
    {
        actions.push(Action::edit(vec![Op::Bind { param: param.clone(), component, from, channel: None }]));
    }
}

/// The link button of a bindable param (`param.<path>.bind`): chan nodes to bind to (with their
/// channels where known; a vector or colour first asks for the component), and unbind.
#[allow(clippy::too_many_arguments)]
fn bind_menu(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    param: &ParamPath,
    spec: &ParamSpec,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let key = param.to_string();
    // Named as the project file names them, from the node's level.
    let level = level_of(input.net, node.id);
    let mut sources: Vec<(NodeId, String)> = input
        .net
        .nodes()
        .filter(|n| n.id != node.id)
        .filter(|n| {
            input.types.desc(&n.kind).is_some_and(|d| d.outputs.iter().any(|o| o.kind == DataKind::Chan))
        })
        .filter_map(|n| Some((n.id, level.write_ref(&input.net.path_of(n.id)?))))
        .collect();
    sources.sort_by_key(|(_, label)| ref_order(label));
    let id = ui.make_persistent_id(("bind", &key));
    let r = icons::small_button(ui, Icon::Bind).on_hover_text("bind to a channel");
    auto.add(format!("param.{key}.bind"), r.rect, "bind");
    egui::Popup::menu(&r).id(id).show(|ui| {
        let n = spec.bind_components();
        if n == 1 {
            source_list(ui, input, &sources, param, None, auto, actions);
        } else {
            for c in 0..n {
                let label = match (&spec.kind, c) {
                    (ParamKind::Color, 0) => "r".to_string(),
                    (ParamKind::Color, 1) => "g".to_string(),
                    (ParamKind::Color, 2) => "b".to_string(),
                    (ParamKind::Color, _) => "a".to_string(),
                    (_, 0) => "x".to_string(),
                    (_, 1) => "y".to_string(),
                    (_, 2) => "z".to_string(),
                    _ => "w".to_string(),
                };
                let r = ui.menu_button(format!("{c} ({label})"), |ui| {
                    source_list(ui, input, &sources, param, Some(c), auto, actions);
                });
                auto.add(format!("bind.component.{c}"), r.response.rect, label);
            }
        }
        ui.separator();
        let listening = input.net.nodes().any(|n| {
            n.kind == penumbra_ops::osc::OSC_IN
                || n.kind == penumbra_ops::keyboard::KEYBOARD_IN
                || n.kind == penumbra_ops::spacemouse::SPACEMOUSE_IN
        });
        let needs = "Needs a chan.osc_in, chan.keyboard_in or chan.spacemouse_in node: Tab > chan > osc_in";
        let r = ui
            .add_enabled(listening && input.can_edit, egui::Button::new("learn from OSC"))
            .on_hover_text(if listening {
                "Bind to the next OSC address that arrives with a number (a vector: one number per component),                  to the next key a chan.keyboard_in lists, or to the 3D mouse's button or axis moved most"
            } else {
                needs
            })
            .on_disabled_hover_text(if listening { "Not while Claude is editing" } else { needs });
        auto.add("bind.learn", r.rect, "learn from OSC");
        if r.clicked() {
            actions.push(Action::LearnOsc(param.to_string()));
            ui.close();
        }
        if node.binds.keys().any(|k| k.param.as_str() == spec.name) {
            ui.separator();
            for part in node.binds.keys().filter(|k| k.param.as_str() == spec.name) {
                let text = match part.component {
                    Some(c) => format!("unbind {c}"),
                    None => "unbind".to_string(),
                };
                let r = ui.button(text);
                auto.add(format!("bind.unbind.{part}"), r.rect, "unbind");
                if r.clicked() {
                    actions.push(Action::edit(vec![Op::Unbind {
                        param: param.clone(),
                        component: part.component,
                    }]));
                    ui.close();
                }
            }
        }
    });
}

/// Chan nodes to bind to, each with its channels when the view has its output (automation ids
/// `bind.<i>` for a node's first channel, `bind.<i>.<channel>` for a named one).
fn source_list(
    ui: &mut Ui,
    input: &Inputs<'_>,
    sources: &[(NodeId, String)],
    param: &ParamPath,
    component: Option<u8>,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    if sources.is_empty() {
        ui.label(RichText::new("no chan nodes in the network").color(theme::TEXT_WEAK));
    }
    let mut chosen: Option<(NodeId, Option<Name>)> = None;
    for (i, (id, path)) in sources.iter().enumerate() {
        let names: Vec<Name> = input
            .outputs
            .get(id)
            .and_then(|o| o.iter().find(|d| d.kind() == DataKind::Chan))
            .and_then(|d| d.get::<Chans>())
            .map(|c| c.names().cloned().collect())
            .unwrap_or_default();
        if names.len() <= 1 {
            let label = match names.first() {
                Some(n) if n.as_str() != "value" => format!("{path}:{n}"),
                _ => path.clone(),
            };
            let r = ui.button(label);
            auto.add(format!("bind.{i}"), r.rect, path.clone());
            if r.clicked() {
                chosen = Some((*id, None));
            }
        } else {
            let r = ui.menu_button(path, |ui| {
                for n in &names {
                    let r = ui.button(n.as_str());
                    auto.add(format!("bind.{i}.{n}"), r.rect, n.to_string());
                    if r.clicked() {
                        chosen = Some((*id, Some(n.clone())));
                    }
                }
            });
            auto.add(format!("bind.{i}"), r.response.rect, path.clone());
        }
    }
    if let Some((id, channel)) = chosen
        && let Some(from) = input.net.path_of(id)
    {
        actions.push(Action::edit(vec![Op::Bind { param: param.clone(), component, from, channel }]));
        ui.close();
    }
}

/// The level a node sits on, from which its references are written.
fn level_of(net: &Network, id: NodeId) -> penumbra_core::NodePath {
    net.path_of(id).and_then(|p| p.parent()).unwrap_or_else(penumbra_core::NodePath::root)
}

/// Sort key of a reference in a menu: nodes on the same level first, then below, then up, then
/// absolute ones.
fn ref_order(label: &str) -> (u8, String) {
    let group = if label.starts_with('/') {
        3
    } else if label.starts_with("..") {
        2
    } else if label.contains('/') {
        1
    } else {
        0
    };
    (group, label.to_lowercase())
}

fn sort_refs(refs: &mut [String]) {
    refs.sort_by_key(|r| ref_order(r));
}

fn vector(c: &[f64]) -> Option<Value> {
    Some(match *c {
        [x, y] => Value::Vec2([x, y]),
        [x, y, z] => Value::Vec3([x, y, z]),
        [x, y, z, w] => Value::Vec4([x, y, z, w]),
        _ => return None,
    })
}

/// Files under `dir` (two levels deep, at most 500) with one of `extensions`, as relative paths
/// with forward slashes.
fn list_files(dir: &Path, extensions: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            if out.len() >= 500 {
                break;
            }
            let p = e.path();
            if p.is_dir() {
                if depth < 2 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let ext = p.extension().and_then(|x| x.to_str()).map(str::to_lowercase).unwrap_or_default();
            if (!extensions.is_empty() && !extensions.contains(&ext)) || ext == "tmp" {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(dir) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// Params of a node as a map, for `ui.state`.
pub fn values(net: &Network, types: &NodeTypes, id: NodeId) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(node) = net.node(id) else { return out };
    if let Some(desc) = types.desc(&node.kind) {
        for spec in &desc.params {
            out.insert(
                spec.name.clone(),
                node.params.get(spec.name.as_str()).unwrap_or(&spec.default).clone(),
            );
        }
    }
    for (name, value) in &node.params {
        out.entry(name.to_string()).or_insert_with(|| value.clone());
    }
    out
}

/// A `tex.shapes`'s trace row: the node whose picture is traced (typed, or picked from the
/// menu) and the button that writes its outlines into `shapes` with the tag `trace`, replacing
/// the lines traced before (`shapes.trace` with its defaults; one undo step).
fn trace_row(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    from: &mut String,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.add_enabled_ui(input.can_edit, |ui| {
            let tip = "Traces the outlines of this node's picture into `poly` lines tagged `trace`; \
                       lines traced before are replaced. Threshold 0.5 on alpha (a cut-out), else on \
                       brightness; specks under 64 pixels dropped, holes filled. shapes.trace has the \
                       other settings";
            let r = ui.add_enabled(!from.trim().is_empty(), egui::Button::new("trace")).on_hover_text(tip);
            auto.add("param.trace", r.rect, "trace");
            if r.clicked() {
                actions.push(Action::Trace { node: input.node, from: from.trim().to_string() });
            }
            let mut picked = None;
            node_menu(ui, input, node, "trace", auto, &mut picked);
            if let Some(Value::Text(p)) = picked {
                *from = p;
            }
            let r = ui.add(
                egui::TextEdit::singleline(from).hint_text("mask node").desired_width(ui.available_width()),
            );
            auto.add("param.trace_from", r.rect, "trace from");
        });
    });
}

/// A `tex.scan`'s scan button (cancel while a scan runs) and how its scan stands: the step and
/// a progress bar while running, then how the last run ended, its error or its warnings.
fn scan_row(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let state = input.scans.state(input.node).unwrap_or_default();
    let busy = input.scans.running();
    ui.horizontal(|ui| {
        if state.running {
            let r = ui.button("cancel").on_hover_text("Stop the scan before its next picture (scan.cancel)");
            auto.add("param.scan_cancel", r.rect, "cancel scan");
            if r.clicked() {
                actions.push(Action::CancelScan);
            }
            let bar = egui::ProgressBar::new(state.progress.unwrap_or(0.0))
                .text(RichText::new(&state.step).small())
                .desired_width(ui.available_width());
            let r = ui.add(bar);
            auto.add("param.scan_progress", r.rect, "scan progress");
            return;
        }
        let tip = "Scans every tex.scan that names the same camera as this one, one projector after \
                   another (about 20 s each; the projector being scanned shows the patterns, the others \
                   black). Then lines the projectors up (when each tex.scan names its mapper) and \
                   blends them (scan.run). Keep the room dark and nothing moving";
        let tip = if busy { "Another scan is running" } else { tip };
        let r = ui
            .add_enabled(input.can_edit && !busy, egui::Button::new("scan"))
            .on_hover_text(tip)
            .on_disabled_hover_text(tip);
        auto.add("param.scan", r.rect, "scan");
        if r.clicked() {
            actions.push(Action::Scan(input.node));
        }
        let (text, colour) = match &state.error {
            Some(e) => (e.clone(), theme::ERROR),
            None if state.step.is_empty() => {
                // Before any run in this session: what the node holds from the files.
                let desc = input.types.desc(&node.kind);
                let text = |name: &str| {
                    desc.and_then(|d| node.param(d, name)).and_then(Value::as_str).unwrap_or("").to_string()
                };
                let dir = input.project_dir.unwrap_or(Path::new("."));
                let folder = penumbra_ops::tex::scan::scan_folder(dir, &text("folder"), &text("projector"));
                let shown =
                    folder.strip_prefix(dir).unwrap_or(&folder).display().to_string().replace('\\', "/");
                if text("projector").trim().is_empty() {
                    ("name the projector to scan".to_string(), theme::WARNING)
                } else if penumbra_ops::tex::scan::has_scan(&folder) {
                    (format!("holds the scan in {shown}"), theme::TEXT_WEAK)
                } else {
                    (format!("no scan in {shown} yet"), theme::WARNING)
                }
            }
            None => (state.step.clone(), theme::TEXT_WEAK),
        };
        ui.add(egui::Label::new(RichText::new(text).color(colour).small()).wrap());
    });
    for w in &state.warnings {
        ui.add(egui::Label::new(RichText::new(w).color(theme::WARNING).small()).wrap());
    }
}

/// An `out.video`'s record or stop button and how its recording stands. The button switches
/// the `record` param (an edit, one undo step), so button and param always agree; while a
/// channel drives `record` the button is off.
fn record_row(
    ui: &mut Ui,
    input: &Inputs<'_>,
    node: &NodeDef,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let status = input.record.status(input.node);
    let bound = node.binds.keys().any(|k| k.param.as_str() == "record");
    let stored = node.params.get("record").and_then(Value::as_bool).unwrap_or(false);
    let live = input.status.and_then(|s| s.bound.get("record")).and_then(Value::as_bool);
    let on = if bound { live.unwrap_or(stored) } else { stored };
    ui.horizontal(|ui| {
        let (label, tip, id) = if on {
            (
                "stop",
                "Turn record off: stop recording and finish the file (video_out.stop)",
                "param.record_stop",
            )
        } else {
            (
                "record",
                "Turn record on: start recording to a new file (video_out.start)",
                "param.record_start",
            )
        };
        let tip = if bound { "record is bound to a channel; unbind record to switch it here" } else { tip };
        let r = ui
            .add_enabled(input.can_edit && !bound, egui::Button::new(label))
            .on_hover_text(tip)
            .on_disabled_hover_text(tip);
        auto.add(id, r.rect, label);
        if r.clicked()
            && let Some(path) = input.net.path_of(input.node)
            && let Ok(name) = "record".parse::<Name>()
            && let Ok(param) = ParamPath::new(path, name)
        {
            actions.push(Action::Edit {
                ops: vec![Op::Set { param, value: Value::Bool(!on) }],
                label: if on { "stop recording" } else { "start recording" }.into(),
                group: None,
            });
        }
        let Some(s) = &status else {
            ui.label(RichText::new("not recording").color(theme::TEXT_WEAK).small());
            return;
        };
        let name = Path::new(&s.file)
            .file_name()
            .map_or_else(|| s.file.clone(), |n| n.to_string_lossy().into_owned());
        let (text, colour) = if s.recording {
            let text = format!(
                "{name}  {:.1} s  {} frames  {} dropped  encoder {:.0} fps",
                s.seconds, s.frames, s.dropped, s.encoder_fps
            );
            (text, if s.dropped > 0 { theme::WARNING } else { theme::ERROR })
        } else if s.finishing {
            (format!("{name}: finishing the file ..."), theme::TEXT_WEAK)
        } else if let Some(e) = &s.error {
            (e.clone(), theme::ERROR)
        } else {
            let text =
                format!("saved {name}: {:.1} s, {} frames, {} dropped", s.seconds, s.frames, s.dropped);
            (text, theme::TEXT_WEAK)
        };
        ui.label(RichText::new(text).color(colour).small()).on_hover_text(&s.file);
    });
}

/// How an `out.audio`'s device plays: device, rate, latency, gaps, what the limiter takes off;
/// and the mute of every output (`audio.mute`), which is not an edit.
fn audio_out_row(ui: &mut Ui, input: &Inputs<'_>, auto: &mut automation::Registry) {
    let out = input.audio_out;
    let lane = out.lane_status(input.node);
    let bus = lane.as_ref().and_then(|(k, _)| out.bus_status(k));
    ui.horizontal(|ui| {
        let muted = out.muted();
        let (label, tip) = if muted {
            ("unmute all", "Every audio output is muted; this unmutes them (audio.mute false)")
        } else {
            ("mute all", "Mutes every audio output at once, a panic button (audio.mute)")
        };
        let r = ui.button(label).on_hover_text(tip);
        auto.add("param.audio_mute", r.rect, label);
        if r.clicked() {
            out.set_muted(!muted);
        }
        let weak = theme::TEXT_WEAK;
        let Some(bus) = bus else {
            ui.label(RichText::new("not playing").color(weak).small());
            return;
        };
        let mut text = String::new();
        let mut colour = weak;
        match &bus.player {
            Some(p) if p.open => {
                text.push_str(&format!(
                    "{}  {} Hz  {:.0} ms ({:.0} queued + {:.0} device)",
                    p.device.as_deref().unwrap_or("?"),
                    p.rate,
                    p.latency_ms,
                    p.engine.fill_ms,
                    p.device_ms
                ));
                if p.engine.underruns > 0 || p.glitches > 0 {
                    text.push_str(&format!("  {} gaps  {} glitches", p.engine.underruns, p.glitches));
                    colour = theme::WARNING;
                }
            }
            Some(p) => {
                // The node's warning has the whole error.
                text.push_str(if p.error.is_some() {
                    "the device does not open"
                } else {
                    "opening the device ..."
                });
                colour = theme::WARNING;
            }
            None => text.push_str("no device (offline)"),
        }
        if bus.paused {
            text.push_str("  paused");
        }
        if let Some((_, l)) = &lane {
            text.push_str(&format!("  limiter -{:.1} dB", l.reduction_held_db));
            if l.reduction_held_db >= 1.0 {
                colour = theme::WARNING;
            }
        }
        // Truncated: a long line would widen the panel and push the rows' right ends out of view.
        ui.add(egui::Label::new(RichText::new(text).color(colour).small()).truncate()).on_hover_text(
            "The device, its rate, the delay from a frame to the speaker, and the most the limiter took off \
             in the last two seconds (audio.status)",
        );
    });
}

/// The NDI sources on the network for a `tex.ndi_in`'s `source` (`param.<path>.menu`, entries
/// `source.first` and `source.<i>`). The list is read every frame while the menu is open, so it
/// fills in as sources answer. Ends with the link to NDI's site its licence asks for.
fn ndi_menu(ui: &mut Ui, key: &str, auto: &mut automation::Registry, new: &mut Option<Value>) {
    let r = ui.menu_button("\u{2026}", |ui| {
        let r = ui.button("the first source").on_hover_text("Leaves `source` empty");
        auto.add("source.first", r.rect, "first source");
        if r.clicked() {
            *new = Some(Value::Text(String::new()));
            ui.close();
        }
        match penumbra_ndi::sources() {
            Err(e) => {
                ui.colored_label(theme::WARNING, e);
            }
            Ok(list) => {
                if list.is_empty() {
                    let text = if penumbra_ndi::settling() {
                        "looking for sources..."
                    } else {
                        "no NDI sources on the network"
                    };
                    ui.label(RichText::new(text).color(theme::TEXT_WEAK));
                }
                for (i, s) in list.iter().enumerate() {
                    let r = ui.button(&s.name).on_hover_text(&s.url);
                    auto.add(format!("source.{i}"), r.rect, s.name.clone());
                    if r.clicked() {
                        *new = Some(Value::Text(s.name.clone()));
                        ui.close();
                    }
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        ui.separator();
        ndi_link(ui, key, auto);
    });
    auto.add(format!("param.{key}.menu"), r.response.rect, "NDI sources");
}

/// The link to NDI's site, which its licence asks for wherever NDI is chosen
/// (`param.<path>.ndi`).
fn ndi_link(ui: &mut Ui, key: &str, auto: &mut automation::Registry) {
    let r = ui
        .hyperlink_to(RichText::new("NDI\u{ae}").color(theme::TEXT_WEAK), penumbra_ndi::SITE)
        .on_hover_text("NDI\u{ae} is a registered trademark of Vizrt NDI AB. ndi.video");
    auto.add(format!("param.{key}.ndi"), r.rect, "ndi.video");
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::NodePath;

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    #[test]
    fn constant_rows_move_up_when_one_goes() {
        let types = penumbra_ops::builtin_types().unwrap();
        let create = |p: &str, kind: &str| Op::Create {
            path: NodePath::parse(p).unwrap(),
            kind: kind.into(),
            params: Default::default(),
        };
        let net = Network::new()
            .apply(
                &types,
                &[
                    create("/lfo", "chan.lfo"),
                    create("/c", "chan.constant"),
                    set("/c.rows", 3_i64),
                    set("/c.name0", "a"),
                    set("/c.value0", 0.1),
                    set("/c.value1", 0.2),
                    set("/c.name2", "z"),
                    set("/c.value2", 0.3),
                    Op::Bind {
                        param: ParamPath::parse("/c.value2").unwrap(),
                        component: None,
                        from: "/lfo".parse().unwrap(),
                        channel: None,
                    },
                ],
            )
            .unwrap()
            .network;
        let path: NodePath = "/c".parse().unwrap();
        let id = net.resolve(&path).unwrap();
        let ops = constant_remove_ops(&net, &path, net.node(id).unwrap(), 0, 3);
        let net = net.apply(&types, &ops).unwrap().network;
        let c = net.node(id).unwrap();
        let get = |n: &str| c.params.get(n).cloned();
        // Row 1 (default name `chan1`) is now row 0 and keeps its channel name.
        assert_eq!(get("name0"), Some(Value::Text("chan1".into())));
        assert_eq!(get("value0"), Some(Value::Float(0.2)));
        assert_eq!(get("name1"), Some(Value::Text("z".into())));
        assert_eq!(get("value1"), Some(Value::Float(0.3)));
        assert_eq!((get("name2"), get("value2")), (None, None));
        assert_eq!(get("rows"), Some(Value::Int(2)));
        let bound: Vec<String> = c.binds.keys().map(ToString::to_string).collect();
        assert_eq!(bound, ["value1"]);
        // Adding a row picks a name no row has.
        let ops = constant_add_ops(&path, c, 2);
        let net = net.apply(&types, &ops).unwrap().network;
        let c = net.node(id).unwrap();
        assert_eq!(c.params.get("rows"), Some(&Value::Int(3)));
        assert_eq!(c.params.get("name2"), None, "chan2 is free");
    }
}
