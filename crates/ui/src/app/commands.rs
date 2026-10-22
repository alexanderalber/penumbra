//! The UI side of the `ui.*` commands: they arrive from the control channel and are handled on
//! the UI thread, between frames. Input commands queue synthetic events and reply once a frame
//! has processed them; `ui.screenshot` replies once a frame has been drawn.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use penumbra_core::{NodeId, NodePath};
use penumbra_graph::Network;
use serde_json::{Value as Json, json};

use super::{Outcome, PenumbraApp};
use crate::Action;
use crate::control::UiRequest;
use crate::editor;
use crate::navigate::{self, Move};
use crate::params;

impl PenumbraApp {
    pub(super) fn handle(&mut self, ctx: &egui::Context, req: &UiRequest) -> Outcome {
        match self.handle_inner(ctx, req) {
            Ok(o) => o,
            Err(e) => Outcome::Done(Err(e)),
        }
    }

    fn handle_inner(&mut self, ctx: &egui::Context, req: &UiRequest) -> Result<Outcome, String> {
        let p = &req.params;
        let now = Instant::now();
        // Checked for every command, as the input commands read them.
        let (modifiers, button) = input_args(p)?;
        let done = |v: Json| Ok(Outcome::Done(Ok(v)));
        match req.method.as_str() {
            "ui.state" => done(self.ui_state(ctx, p, now)),
            "ui.elements" => {
                let prefix = p.get("prefix").and_then(Json::as_str).unwrap_or("");
                done(json!({"elements": self.auto.query(prefix)}))
            }
            "ui.click" => self.click(p, modifiers, button),
            "ui.drag" => self.drag(p, modifiers, button),
            "ui.scroll" => self.scroll(p, modifiers),
            "ui.key" => self.key(p, modifiers),
            "ui.type" => {
                let text = p.get("text").and_then(Json::as_str).ok_or("`text` is missing")?;
                self.synthetic.push_back(egui::Event::Text(text.to_string()));
                Ok(Outcome::AfterInput)
            }
            "ui.select" => done(self.select(p, now)?),
            "ui.enter" => done(self.enter(p, now)?),
            "ui.view" => {
                let net = self.engine.session().doc().snapshot();
                let pinned = match p.get("path").and_then(Json::as_str) {
                    Some(path) => Some(resolve(&net, path)?),
                    None => None,
                };
                if let Some(slot) = p.get("output") {
                    let id = pinned.ok_or("`output` needs a `path`")?;
                    let session = self.engine.session();
                    let names: Vec<&str> = net
                        .node(id)
                        .and_then(|n| session.types().desc(&n.kind))
                        .map(|d| d.outputs.iter().map(|o| o.name.as_str()).collect())
                        .unwrap_or_default();
                    let index = match (slot.as_str(), slot.as_u64()) {
                        (Some(name), _) => names.iter().position(|n| *n == name),
                        (None, Some(i)) => usize::try_from(i).ok().filter(|i| *i < names.len()),
                        _ => None,
                    };
                    let index = index.ok_or_else(|| {
                        format!("no output {slot} on the node; it has {}", names.join(", "))
                    })?;
                    self.viewer.output = u32::try_from(index).map_err(|_| "too many outputs")?;
                }
                self.viewer.pinned = pinned;
                done(json!({"pinned": pinned.and_then(|id| path_of(&net, id)), "output": self.viewer.output}))
            }
            "ui.camera" => done(self.camera(p, now)?),

            "ui.options" => done(self.options(p, now)?),
            "ui.arrange" => done(self.arrange(p, now)?),
            "ui.measure" => {
                let session = self.engine.session();
                let net = session.doc().snapshot();
                let m = editor::measure(&net, session.types(), self.editor.level, &self.layout);
                let round = |v: f32| (v * 10.0).round() / 10.0;
                done(json!({
                    "nodes": m.nodes,
                    "wires": m.wires,
                    "width": round(m.width),
                    "height": round(m.height),
                    "wire_length": round(m.wire_length),
                    "longest_wire": round(m.longest),
                    "crossings": m.crossings,
                    "crossings_at": m.crossings_at.iter().map(|p| [round(p.x), round(p.y)]).collect::<Vec<_>>(),
                    "overlap": round(m.overlap),
                    "overlaps": m.overlaps.iter().map(|p| [round(p.x), round(p.y)]).collect::<Vec<_>>(),
                    "through_nodes": round(m.through_nodes),
                }))
            }
            "ui.frame" => {
                self.editor.frame_all();
                done(json!({}))
            }
            "ui.screenshot" => {
                let file = match p.get("file").and_then(Json::as_str) {
                    Some(f) => PathBuf::from(f),
                    None => std::env::temp_dir().join("penumbra").join("ui.png"),
                };
                if ctx.input(|i| i.viewport().minimized == Some(true)) {
                    return Err("the window is minimised; nothing is drawn".into());
                }
                Ok(Outcome::Screenshot(file))
            }
            other => Err(format!("unknown ui command `{other}`")),
        }
    }

    fn element_pos(&self, p: &Json) -> Result<Pos2, String> {
        if let Some(id) = p.get("id").and_then(Json::as_str) {
            return self
                .auto
                .find(id)
                .map(|e| e.rect().center())
                .ok_or_else(|| format!("no element `{id}` on screen; `ui.elements` lists them"));
        }
        match (p.get("x").and_then(Json::as_f64), p.get("y").and_then(Json::as_f64)) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok(pos2(x as f32, y as f32)),
            _ => Err("give `id` or `x` and `y`".into()),
        }
    }

    fn ui_state(&self, ctx: &egui::Context, p: &Json, now: Instant) -> Json {
        let session = self.engine.session().clone();
        let net = session.doc().snapshot();
        let path = |id: NodeId| path_of(&net, id);
        let selection: Vec<Option<String>> = self.editor.selection.iter().map(|id| path(*id)).collect();
        let selected = self.selected();
        let mut state = json!({
            "selection": selection,
            "viewer": self.viewer.target(&net, selected).and_then(path),
            "pinned": self.viewer.pinned.and_then(path),
            "view": {"origin": [self.layout.view.origin.x, self.layout.view.origin.y], "zoom": self.layout.view.zoom},
            "canvas": rect_json(self.editor.canvas),
            "window": rect_json(ctx.content_rect()),
            "editing": session.status(now).edit,
            "thumbnails_paused": self.editor.paused.iter().filter_map(|id| path(*id)).collect::<Vec<_>>(),
            "level": editor::level_path(&net, self.editor.level),
            "message": self.message.as_ref().map(|(m, _)| m.clone()),
        });
        if let Some(id) = selected
            && let Some(o) = state.as_object_mut()
        {
            o.insert("params".into(), json!(params::values(&net, session.types(), id)));
        }
        if p.get("positions").and_then(Json::as_bool).unwrap_or(false)
            && let Some(o) = state.as_object_mut()
        {
            o.insert("positions".into(), json!(self.layout.positions(&net)));
        }
        state
    }

    fn click(&mut self, p: &Json, modifiers: Modifiers, button: PointerButton) -> Result<Outcome, String> {
        let at = self.element_pos(p)?;
        let count = if p.get("double").and_then(Json::as_bool).unwrap_or(false) { 2 } else { 1 };
        self.synthetic.push_back(egui::Event::PointerMoved(at));
        for _ in 0..count {
            self.synthetic.push_back(egui::Event::PointerButton {
                pos: at,
                button,
                pressed: true,
                modifiers,
            });
            self.synthetic.push_back(egui::Event::PointerButton {
                pos: at,
                button,
                pressed: false,
                modifiers,
            });
        }
        Ok(Outcome::AfterInput)
    }

    fn drag(&mut self, p: &Json, modifiers: Modifiers, button: PointerButton) -> Result<Outcome, String> {
        let from = self.element_pos(p.get("from").unwrap_or(&Json::Null))?;
        let to = self.element_pos(p.get("to").unwrap_or(&Json::Null))?;
        let steps = p.get("steps").and_then(Json::as_u64).unwrap_or(8).clamp(2, 200);
        self.synthetic.push_back(egui::Event::PointerMoved(from));
        self.synthetic.push_back(egui::Event::PointerButton { pos: from, button, pressed: true, modifiers });
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            self.synthetic.push_back(egui::Event::PointerMoved(from + (to - from) * t));
        }
        self.synthetic.push_back(egui::Event::PointerButton { pos: to, button, pressed: false, modifiers });
        Ok(Outcome::AfterInput)
    }

    fn scroll(&mut self, p: &Json, modifiers: Modifiers) -> Result<Outcome, String> {
        let at = self.element_pos(p)?;
        let dy = p.get("dy").and_then(Json::as_f64).filter(|d| d.is_finite()).unwrap_or(0.0) as f32;
        self.synthetic.push_back(egui::Event::PointerMoved(at));
        self.synthetic.push_back(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers,
        });
        Ok(Outcome::AfterInput)
    }

    fn key(&mut self, p: &Json, modifiers: Modifiers) -> Result<Outcome, String> {
        let name = p.get("key").and_then(Json::as_str).ok_or("`key` is missing")?;
        let key = Key::from_name(name).ok_or_else(|| format!("unknown key `{name}`"))?;
        // As the window's integration does: Ctrl+C, X, V become clipboard events, the paste with
        // what was copied last (the system clipboard is not read here).
        if modifiers.command {
            let event = match key {
                Key::C => Some(egui::Event::Copy),
                Key::X => Some(egui::Event::Cut),
                Key::V => penumbra_engine::clipboard::copied_text(&self.engine).map(egui::Event::Paste),
                _ => None,
            };
            if let Some(event) = event {
                self.synthetic.push_back(event);
                return Ok(Outcome::AfterInput);
            }
        }
        for pressed in [true, false] {
            self.synthetic.push_back(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
        }
        Ok(Outcome::AfterInput)
    }

    fn select(&mut self, p: &Json, now: Instant) -> Result<Json, String> {
        let net = self.engine.session().doc().snapshot();
        let paths = p.get("paths").and_then(Json::as_array).ok_or("`paths` must be a list")?;
        let mut set = BTreeSet::new();
        for path in paths {
            set.insert(resolve(&net, path.as_str().ok_or("paths are strings")?)?);
        }
        // The editor shows one component's inside at a time: the one holding the nodes.
        let level = set.iter().next().and_then(|id| net.node(*id)).and_then(|n| n.parent);
        if set.iter().any(|id| net.node(*id).is_some_and(|n| n.parent != level)) {
            return Err("the nodes must sit side by side (in the same component)".into());
        }
        if !set.is_empty() {
            self.editor.enter(level, &mut self.layout, now);
        }
        self.editor.selection = set;
        Ok(json!({"selected": paths}))
    }

    fn enter(&mut self, p: &Json, now: Instant) -> Result<Json, String> {
        let session = self.engine.session().clone();
        let net = session.doc().snapshot();
        let level = match p.get("path").and_then(Json::as_str).filter(|s| *s != "/") {
            Some(path) => {
                let id = resolve(&net, path)?;
                let container =
                    net.node(id).and_then(|n| session.types().desc(&n.kind)).is_some_and(|d| d.container);
                if !container {
                    return Err(format!("{path} is not a component"));
                }
                Some(id)
            }
            None => None,
        };
        self.editor.enter(level, &mut self.layout, now);
        Ok(json!({"level": editor::level_path(&net, level)}))
    }

    fn camera(&mut self, p: &Json, now: Instant) -> Result<Json, String> {
        let session = self.engine.session().clone();
        let net = session.doc().snapshot();
        let selected = self.selected();
        let id = self.viewer.target(&net, selected).ok_or("the viewer shows nothing")?;
        let types = session.types();
        let scene = navigate::scene(&net, types, id, self.viewer.output)
            .ok_or("the viewer does not show a render or geometry")?;
        let nav = &mut self.viewer.nav;
        if let Some(mode) = p.get("mode") {
            let mode: navigate::Mode =
                serde_json::from_value(mode.clone()).map_err(|_| "`mode` is picture, free or steer")?;
            if mode == navigate::Mode::Steer
                && let Err(why) = &scene.steer
            {
                return Err(why.clone());
            }
            nav.set_mode(id, mode);
        }
        if p.get("home").and_then(Json::as_bool).unwrap_or(false) {
            nav.home(id);
        }
        if let Some(c) = p.get("camera") {
            let o: penumbra_ops::view::Orbit = serde_json::from_value(c.clone())
                .map_err(|e| format!("`camera` needs eye, target, roll, fov: {e}"))?;
            if !o.is_valid() {
                return Err("`camera` needs finite numbers and an eye apart from the target".into());
            }
            nav.set_camera(id, o);
        }
        let m = camera_move(p)?;
        // The command is who steers, whoever holds the editing mode.
        let can_edit = true;
        let doc = session.doc();
        let new_group = || doc.new_group();
        let mut actions = Vec::new();
        if !m.is_none() {
            if nav.mode(&scene) == navigate::Mode::Free && nav.camera(id).is_none() {
                return Err("the free view has no camera yet (its first picture brings it)".into());
            }
            nav.apply(&m, &scene, &net, types, can_edit, &new_group, now, &mut actions)?;
            // A command's move is a movement of its own.
            nav.settle(now + Duration::from_secs(1), false);
        }
        if p.get("take").and_then(Json::as_bool).unwrap_or(false) {
            let ops = nav.take_view(&scene, &net)?;
            actions.push(Action::Edit { ops, label: "take the view".into(), group: None });
        }
        let mode = nav.mode(&scene);
        let camera = nav.camera(id);
        let locked = nav.horizon_locked;
        let steer = scene.steer.clone().ok().and_then(|c| path_of(&net, c));
        self.apply(actions, now);
        let net = session.doc().snapshot();
        Ok(json!({
            "mode": mode,
            "camera": camera,
            "steers": steer,
            "steered": scene.steer.ok().and_then(|c| navigate::stored_orbit(&net, types, c)),
            "horizon_locked": locked,
        }))
    }

    /// Arranges the nodes named in `nodes` (grouped by the level they are on), or every level,
    /// as one edit from the control channel.
    fn arrange(&mut self, p: &Json, now: Instant) -> Result<Json, String> {
        let session = self.engine.session();
        let net = session.doc().snapshot();
        let types = session.types();
        // Nodes placed by nobody yet get their place first, so the arrangement starts from
        // what the editor shows.
        self.layout.sync(&net);
        let placed = self.layout.take_placed();
        let net = if placed.is_empty() { net } else { session.place_unplaced(&placed).map_err(|e| e.0)? };
        self.layout.sync(&net);
        let to = match p.get("nodes") {
            None => {
                self.editor.frame_all();
                crate::arrange::everything(&net, types, &self.layout)
            }
            Some(list) => {
                let list = list.as_array().ok_or("`nodes` must be a list of paths")?;
                let mut levels: BTreeMap<Option<NodeId>, BTreeSet<NodeId>> = BTreeMap::new();
                for path in list {
                    let path = path.as_str().ok_or("`nodes` must be a list of paths")?;
                    let id = resolve(&net, path)?;
                    let parent = net.node(id).and_then(|n| n.parent);
                    levels.entry(parent).or_default().insert(id);
                }
                levels
                    .iter()
                    .flat_map(|(level, only)| {
                        crate::arrange::level(&net, types, &self.layout, *level, Some(only))
                    })
                    .collect()
            }
        };
        let ops = crate::layout::place_ops(&net, &to);
        if ops.is_empty() {
            return Ok(json!({"moved": 0, "history": "no_change"}));
        }
        let out =
            session.edit(penumbra_project::Actor::Claude, &ops, None, "arrange", now).map_err(|e| e.0)?;
        let mut reply = penumbra_engine::commands::outcome(&out);
        if let Some(o) = reply.as_object_mut() {
            o.insert("moved".into(), json!(ops.len()));
        }
        Ok(reply)
    }

    fn options(&mut self, p: &Json, now: Instant) -> Result<Json, String> {
        let mut options = self.layout.options;
        if let Some(v) = p.get("chan_bars") {
            options.chan_bars = v.as_bool().ok_or("`chan_bars` must be true or false")?;
        }
        if let Some(v) = p.get("viewer_background") {
            options.viewer_background = v.as_bool().ok_or("`viewer_background` must be true or false")?;
        }
        if let Some(v) = p.get("binding_lines") {
            options.binding_lines = v.as_bool().ok_or("`binding_lines` must be true or false")?;
        }
        if let Some(v) = p.get("chan_scale") {
            options.chan_scale = v
                .as_str()
                .and_then(crate::layout::ChanScale::parse)
                .ok_or("`chan_scale` must be auto, shared or per_channel")?;
        }
        if let Some(v) = p.get("audio_view") {
            options.audio_view = v
                .as_str()
                .and_then(crate::layout::AudioView::parse)
                .ok_or("`audio_view` must be meters or plot")?;
        }
        let aspect = match p.get("viewer_aspect") {
            None => self.layout.viewer_aspect,
            Some(Json::String(s)) if s == "auto" => None,
            Some(v) => Some(
                v.as_f64()
                    .map(|a| a as f32)
                    .filter(|a| a.is_finite() && *a > 0.0)
                    .ok_or("`viewer_aspect` must be \"auto\" or a width over height above 0")?,
            ),
        };
        self.layout.set_options(options, now);
        self.layout.set_viewer_aspect(aspect, now);
        self.viewer.chan_scale = options.chan_scale;
        self.window_viewer.chan_scale = options.chan_scale;
        self.viewer.audio_view = options.audio_view;
        self.window_viewer.audio_view = options.audio_view;
        Ok(json!({
            "chan_bars": options.chan_bars,
            "viewer_background": options.viewer_background,
            "binding_lines": options.binding_lines,
            "chan_scale": options.chan_scale.name(),
            "audio_view": options.audio_view.name(),
            "viewer_aspect": self.layout.viewer_aspect.map_or(json!("auto"), |a| json!(a)),
        }))
    }
}

/// The modifiers (`ctrl`, `shift`, `alt`) and the pointer button (`button`: left, right or
/// middle) of an input command.
fn input_args(p: &Json) -> Result<(Modifiers, PointerButton), String> {
    let modifiers = Modifiers {
        ctrl: p.get("ctrl").and_then(Json::as_bool).unwrap_or(false),
        command: p.get("ctrl").and_then(Json::as_bool).unwrap_or(false),
        shift: p.get("shift").and_then(Json::as_bool).unwrap_or(false),
        alt: p.get("alt").and_then(Json::as_bool).unwrap_or(false),
        mac_cmd: false,
    };
    let button = match p.get("button").and_then(Json::as_str) {
        None | Some("left") => PointerButton::Primary,
        Some("right") => PointerButton::Secondary,
        Some("middle") => PointerButton::Middle,
        Some(other) => return Err(format!("unknown button `{other}`; left, right or middle")),
    };
    Ok((modifiers, button))
}

/// The move `ui.camera` asks for: orbit, pan, dolly, travel, look, roll.
fn camera_move(p: &Json) -> Result<Move, String> {
    let mut m = Move::none();
    if let Some([y, x]) = numbers::<2>(p, "orbit")? {
        m.orbit = [y, x];
    }
    if let Some(v) = numbers::<2>(p, "pan")? {
        m.pan = v;
    }
    if let Some([d]) = numbers::<1>(p, "dolly")? {
        m.dolly = d;
    }
    if let Some(v) = numbers::<3>(p, "travel")? {
        m.travel = v;
    }
    if let Some(v) = numbers::<2>(p, "look")? {
        m.look = v;
    }
    if let Some([r]) = numbers::<1>(p, "roll")? {
        m.roll = r;
    }
    Ok(m)
}

fn path_of(net: &Network, id: NodeId) -> Option<String> {
    net.path_of(id).map(|p| p.to_string())
}

fn resolve(net: &Network, s: &str) -> Result<NodeId, String> {
    let path: NodePath = s.parse().map_err(|e| format!("{e}"))?;
    net.resolve(&path).ok_or_else(|| format!("no node at {path}"))
}

/// `p[name]`: `N` finite numbers (one may come bare); `None` when absent.
fn numbers<const N: usize>(p: &Json, name: &str) -> Result<Option<[f64; N]>, String> {
    let Some(v) = p.get(name) else { return Ok(None) };
    let list: Option<Vec<f64>> = match v {
        Json::Array(a) => a.iter().map(Json::as_f64).collect(),
        other => other.as_f64().map(|x| vec![x]),
    };
    list.filter(|l| l.iter().all(|x| x.is_finite()))
        .and_then(|l| <[f64; N]>::try_from(l).ok())
        .map(Some)
        .ok_or_else(|| format!("`{name}` must be {N} number(s)"))
}

fn rect_json(r: egui::Rect) -> Json {
    json!([r.min.x, r.min.y, r.width(), r.height()])
}
