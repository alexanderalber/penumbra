//! The "Helpers" window: the outputs (displays, windows, projectors, what paces the show clock),
//! the resolution analysis with its overlay and optimise button, and the show-machine checks with a
//! fix and a restore button each, the persistent state (when it was last saved, save now, restore)
//! and OSC (listeners, senders, what arrived, learn). Opened from the status bar; the same things
//! are `output.list`, `res.analyze`, `res.overlay`, `res.optimize`, `system.check`, `system.fix`,
//! `system.restore`, `state.info`, `state.save`, `state.restore`, `state.discard`, `state.config`,
//! `project.settings`, `osc.status`, `osc.config` and `osc.learn` on the control channel.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use egui::{RichText, Ui};
use penumbra_core::{Name, Value};
use penumbra_core::{PaceSource, lock};
use penumbra_engine::{Engine, ViewFrame};
use penumbra_graph::{Network, Op, PERSIST_STATE};
use penumbra_ops::out::BoardState;
use penumbra_ops::res::{Report, Verdict};
use penumbra_system::{Check, CheckStatus, Previous};

use crate::{Action, automation, theme};

/// What the Helpers window needs besides its own state.
pub struct HelpersInput<'a> {
    pub board: &'a BoardState,
    pub net: &'a std::sync::Arc<Network>,
    pub view: &'a ViewFrame,
    pub engine: &'a std::sync::Arc<Engine>,
    pub can_edit: bool,
}

/// How often the checks are read again while the window is closed (for the status bar).
const RECHECK: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Helpers {
    pub open: bool,
    checks: Option<(Instant, Vec<Check>)>,
    /// What each fix replaced, for its restore button (this run only).
    previous: BTreeMap<String, Previous>,
    message: Option<String>,
    /// What the last save or restore started here said (they run on a thread of their own).
    state_message: Arc<Mutex<Option<(bool, String)>>>,
    /// This machine's address on the network, looked up now and then (for setting up a phone).
    local_ip: Option<(Instant, Option<String>)>,
    /// The audio inputs of this machine, listed on first view and on request (listing takes
    /// some milliseconds, too long for every frame).
    audio_inputs: Option<Result<Vec<penumbra_audio::DeviceInfo>, String>>,
}

impl Helpers {
    fn checks(&mut self, now: Instant, max_age: Duration) -> &[Check] {
        if self.checks.as_ref().is_none_or(|(at, _)| now.saturating_duration_since(*at) >= max_age) {
            self.checks = Some((now, penumbra_system::checks::run()));
        }
        self.checks.as_ref().map(|(_, c)| c.as_slice()).unwrap_or_default()
    }

    /// Checks that found a problem, for the status bar.
    pub fn problems(&mut self, now: Instant) -> usize {
        self.checks(now, RECHECK).iter().filter(|c| c.status == CheckStatus::Problem).count()
    }

    /// Opens the window (from the editor's options menu), with fresh checks.
    pub fn open(&mut self) {
        self.open = true;
        self.checks = None;
    }

    /// The status bar entry: outputs, pacing, problems. Only shown, the window opens from the
    /// editor's options (right-click on empty canvas).
    pub fn status(
        &mut self,
        ui: &mut Ui,
        board: &BoardState,
        pace: Option<PaceSource>,
        auto: &mut automation::Registry,
    ) {
        let windows = board.windows.len();
        let mut text = match windows {
            0 => "no outputs".to_string(),
            1 => "1 output".to_string(),
            n => format!("{n} outputs"),
        };
        text.push_str(match pace {
            Some(PaceSource::Vsync) => " \u{b7} vsync",
            _ => " \u{b7} timer",
        });
        let problems = self.problems(Instant::now());
        let errors = board.projectors.values().filter(|p| p.error.is_some()).count();
        let mut label = RichText::new(text);
        if problems + errors > 0 {
            label = label.color(theme::WARNING);
        }
        let r = ui.label(label).on_hover_text(
            "Outputs and what paces the show clock. Details and show-machine checks: right-click on \
             empty canvas > Helpers",
        );
        auto.add("status.outputs", r.rect, "outputs");
        if problems > 0 {
            ui.colored_label(theme::WARNING, if problems == 1 { "1 to fix" } else { "some to fix" });
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        input: &HelpersInput<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        // As tall as its content up to most of the window (egui's scroll area stopped at about
        // 400 points, and the last section was cut off).
        let tallest = (ctx.content_rect().height() - 120.0).max(200.0);
        let r = egui::Window::new("Helpers")
            .open(&mut open)
            .default_width(620.0)
            .default_height(tallest)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(tallest).show(ui, |ui| {
                    self.outputs(ui, input.board, input.net, input.view.pace);
                    ui.separator();
                    resolution(ui, input, auto, actions);
                    ui.separator();
                    self.state(ui, input, auto, actions);
                    ui.separator();
                    self.osc(ui, input.engine, auto);
                    ui.separator();
                    self.audio(ui, input.engine, auto);
                    ui.separator();
                    self.machine(ui, auto);
                });
            });
        if let Some(r) = r {
            auto.add("helpers", r.response.rect, "Helpers");
        }
        self.open = open;
    }

    fn outputs(&mut self, ui: &mut Ui, board: &BoardState, net: &Network, pace: Option<PaceSource>) {
        ui.heading("Outputs");
        ui.label(
            RichText::new(match pace {
                Some(PaceSource::Vsync) => "The show clock follows the vsync of the window marked \u{2022}.",
                _ => "The show clock runs on a timer (no output window at the show rate).",
            })
            .color(theme::TEXT_WEAK),
        );
        if !board.managed {
            ui.label("This process opens no output windows.");
            return;
        }
        egui::Grid::new("displays").striped(true).num_columns(4).show(ui, |ui| {
            for h in ["display", "mode", "window", ""] {
                ui.label(RichText::new(h).color(theme::TEXT_WEAK));
            }
            ui.end_row();
            for d in &board.displays {
                ui.label(&d.id).on_hover_text(format!(
                    "{} ({}, reported as {})\n{}",
                    d.monitor, d.port, d.connector, d.os_name
                ));
                let hz = d.refresh_hz.map_or(String::new(), |hz| format!(" @ {hz:.2} Hz"));
                ui.label(format!("{}x{}{hz}", d.size[0], d.size[1]));
                match board.windows.iter().find(|w| w.display == d.id) {
                    Some(w) => {
                        let text = format!(
                            "{}{:.1} fps, {} missed, max {:.1} ms",
                            if w.paces { "\u{2022} " } else { "" },
                            w.fps,
                            w.missed,
                            w.max_ms
                        );
                        match &w.error {
                            Some(e) => ui.colored_label(theme::ERROR, e),
                            None => ui.label(text),
                        };
                    }
                    None => {
                        ui.label(RichText::new("none").color(theme::TEXT_WEAK));
                    }
                }
                if d.editor {
                    ui.label(RichText::new("editor").color(theme::TEXT_WEAK));
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
        if board.projectors.is_empty() {
            ui.label(RichText::new("No active projector nodes (out.projector).").color(theme::TEXT_WEAK));
        }
        for (id, p) in &board.projectors {
            let path = net.path_of(*id).map(|p| p.to_string()).unwrap_or_else(|| "?".into());
            ui.horizontal_wrapped(|ui| {
                // Its colour in the test pattern (`test` on the node), to tell them apart.
                let [r, g, b] = penumbra_ops::out::tint(penumbra_ops::out::rank(net, *id).unwrap_or(0));
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(2),
                    egui::Color32::from_rgb(c(r), c(g), c(b)),
                );
                ui.monospace(path);
                if let Some(e) = &p.error {
                    ui.colored_label(theme::ERROR, e);
                } else if let Some(d) = &p.display {
                    ui.label(format!("on {d}"));
                }
                if let Some(w) = &p.warning {
                    ui.colored_label(theme::WARNING, w);
                }
            });
        }
    }

    /// Persistent state.
    fn state(
        &mut self,
        ui: &mut Ui,
        input: &HelpersInput<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let engine = input.engine;
        ui.heading("State");
        let mut persist = input.net.switch(PERSIST_STATE);
        let r = ui
            .add_enabled(input.can_edit, egui::Checkbox::new(&mut persist, "keep node state across restarts"))
            .on_hover_text(
                "Feedback loops, held caches and other nodes continue where they were at the next \
                 start (project setting persist_state). Locked nodes keep their outputs either way",
            );
        auto.add("helpers.state.persist", r.rect, "keep node state across restarts");
        if r.changed()
            && let Ok(name) = Name::new(PERSIST_STATE)
        {
            actions.push(Action::edit(vec![Op::Setting { name, value: Some(Value::Bool(persist)) }]));
        }
        let shared = engine.state();
        let status = shared.status();
        let mut config = shared.config();
        let weak = theme::TEXT_WEAK;
        let saved = match &status.last_save {
            Some(r) => {
                let at = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(r.unix_time.max(0.0));
                let age = SystemTime::now().duration_since(at).map_or(0, |d| d.as_secs());
                let nodes = match r.nodes.len() {
                    0 => "nothing to keep".to_string(),
                    1 => "1 node".to_string(),
                    n => format!("{n} nodes"),
                };
                format!("saved {age} s ago: {nodes}, {:.1} MB", r.bytes as f64 / 1e6)
            }
            None if status.holding => "waiting to read the saved state back".to_string(),
            None => "not saved yet".to_string(),
        };
        ui.label(saved).on_hover_text(engine.state_dir().display().to_string());
        if let Some(r) = &status.restore {
            let text = match (&r.file, r.restored.len()) {
                (Some(_), 0) => "restored nothing (no node fits)".to_string(),
                (Some(_), n) => format!("restored {n} at the start: {}", r.restored.join(", ")),
                (None, _) => r.note.clone().unwrap_or_else(|| "nothing restored".into()),
            };
            ui.label(RichText::new(text).color(weak));
            for (file, why) in &r.broken {
                ui.colored_label(theme::WARNING, format!("{file}: {why}"));
            }
        }
        if let Some((_, e)) = &status.last_error {
            ui.colored_label(theme::WARNING, format!("last failure: {e}"));
        }
        ui.horizontal(|ui| {
            ui.label("every").on_hover_text(
                "How often what the nodes hold is written to the project's state folder (and when closing)",
            );
            let mut seconds = config.interval.as_secs_f64();
            let d = ui.add(egui::DragValue::new(&mut seconds).range(1.0..=86_400.0).suffix(" s").speed(1.0));
            auto.add("helpers.state.interval", d.rect, "interval");
            if d.changed() {
                config.interval = Duration::from_secs_f64(seconds.clamp(1.0, 86_400.0));
                shared.set_config(config.clone());
            }
            let r = ui.button("Save now").on_hover_text("state.save");
            auto.add("helpers.state.save", r.rect, "Save now");
            if r.clicked() {
                let (engine, out) = (engine.clone(), self.state_message.clone());
                spawn("state-save", move || {
                    let m = match engine.save_state(Duration::from_secs(20)) {
                        Ok(r) => {
                            (true, format!("saved {} nodes, {:.1} MB", r.nodes.len(), r.bytes as f64 / 1e6))
                        }
                        Err(e) => (false, e.0),
                    };
                    *lock(&out) = Some(m);
                });
            }
            let r = ui.button("Restore").on_hover_text(
                "Give feedback loops, caches and locked nodes what was saved last (state.restore)",
            );
            auto.add("helpers.state.restore", r.rect, "Restore");
            if r.clicked() {
                let (engine, out) = (engine.clone(), self.state_message.clone());
                spawn("state-restore", move || {
                    let m = match engine.restore_state(None) {
                        Ok(r) if r.file.is_some() => (true, format!("restored {}", r.restored.len())),
                        Ok(r) => (false, r.note.unwrap_or_else(|| "nothing restored".into())),
                        Err(e) => (false, e.0),
                    };
                    *lock(&out) = Some(m);
                });
            }
            let r = ui.button("Start fresh").on_hover_text(
                "Feedback loops, caches and other nodes forget what they hold, here and in the state \
                 folder; locked nodes keep their outputs (state.discard; --fresh at the start)",
            );
            auto.add("helpers.state.discard", r.rect, "Start fresh");
            if r.clicked() {
                let (engine, out) = (engine.clone(), self.state_message.clone());
                spawn("state-discard", move || {
                    let m = match engine.discard_state() {
                        Ok(m) => (true, m),
                        Err(e) => (false, e.0),
                    };
                    *lock(&out) = Some(m);
                });
            }
        });
        if let Some((ok, m)) = lock(&self.state_message).as_ref() {
            ui.colored_label(if *ok { weak } else { theme::WARNING }, m);
        }
    }

    /// OSC.
    fn osc(&mut self, ui: &mut Ui, engine: &Arc<Engine>, auto: &mut automation::Registry) {
        ui.heading("OSC");
        let weak = theme::TEXT_WEAK;
        let osc = engine.osc();
        let hub = osc.hub();
        let status = hub.status();
        let now = Instant::now();
        if self
            .local_ip
            .as_ref()
            .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= Duration::from_secs(10))
        {
            self.local_ip = Some((now, penumbra_osc::hub::local_ip().map(|ip| ip.to_string())));
        }
        let ip = self.local_ip.as_ref().and_then(|(_, ip)| ip.clone());
        ui.horizontal(|ui| {
            let mut config = hub.config();
            let r = ui.checkbox(&mut config.enabled, "listen").on_hover_text(
                "Off closes every OSC port for this run (osc.config enabled); the chan.oscin nodes stay",
            );
            auto.add("helpers.osc.enabled", r.rect, "listen");
            if r.changed() {
                hub.set_config(config);
            }
            let text = match &ip {
                Some(ip) => format!("this machine on the network: {ip}"),
                None => "no network found".to_string(),
            };
            ui.label(RichText::new(text).color(weak)).on_hover_text(
                "The address to enter on a phone or another computer; it reaches a chan.oscin only with lan on",
            );
        });
        if status.listeners.is_empty() && status.senders.is_empty() {
            ui.label(
                RichText::new("Nothing listens or sends: add a chan.oscin or chan.oscout node (Tab > chan).")
                    .color(weak),
            );
        }
        for l in &status.listeners {
            let reach = if l.lan { "network" } else { "this machine only" };
            let mut text = format!("port {} \u{b7} {reach}", l.port);
            if let Some(b) = &l.bound
                && l.port == 0
            {
                text.push_str(&format!(" ({b})"));
            }
            text.push_str(&format!(" \u{b7} {} packets \u{b7} {} addresses", l.packets, l.addresses));
            if l.params {
                text.push_str(" \u{b7} sets params");
            }
            ui.label(text);
            if let Some(e) = &l.error {
                ui.colored_label(theme::WARNING, e);
            }
            let mut dropped = Vec::new();
            for (n, what) in [
                (l.malformed, "malformed"),
                (l.oversize, "too large"),
                (l.rate_dropped, "over the rate"),
                (l.pattern_dropped, "patterns over budget"),
                (l.store_full, "addresses over the limit"),
            ] {
                if n > 0 {
                    dropped.push(format!("{n} {what}"));
                }
            }
            if !dropped.is_empty() {
                let r = ui.colored_label(theme::WARNING, format!("dropped: {}", dropped.join(", ")));
                if let Some(m) = &l.last_malformed {
                    r.on_hover_text(m);
                }
            }
        }
        for s in &status.senders {
            let mut text = format!("sending to {}: {} packets", s.target, s.packets);
            if s.errors > 0 {
                text.push_str(&format!(", {} failed", s.errors));
            }
            let r = ui.label(text);
            if let Some(e) = &s.last_error {
                r.on_hover_text(e);
            }
        }
        let stats = osc.stats();
        if stats.applied + stats.refused > 0 {
            let text = format!("params by address: {} set, {} refused", stats.applied, stats.refused);
            let r = ui.label(text);
            if let Some(why) = &stats.last_refused {
                r.on_hover_text(format!("last refused: {why}"));
            }
        }
        if let Some(l) = osc.learning() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    theme::kind_color(penumbra_graph::DataKind::Chan),
                    format!("learning {}: move a control ({:.0} s)", l.param, l.remaining),
                );
                let r = ui.button("Cancel");
                auto.add("helpers.osc.learn_cancel", r.rect, "Cancel");
                if r.clicked() {
                    osc.cancel_learn();
                }
            });
        } else if let Some(l) = osc.learned() {
            match (&l.error, &l.address) {
                (Some(e), _) => {
                    ui.colored_label(theme::WARNING, format!("learn {}: {e}", l.param));
                }
                (None, Some(a)) => {
                    let text = format!(
                        "learned {} from {a} ({}: {})",
                        l.param,
                        l.from.as_deref().unwrap_or("?"),
                        l.channels.join(", ")
                    );
                    ui.label(RichText::new(text).color(weak));
                }
                _ => {}
            }
        }
        if !status.recent.is_empty() {
            ui.label(RichText::new("last messages, the latest per address").color(weak));
            for r in status.recent.iter().take(8) {
                ui.label(
                    RichText::new(format!(
                        "{:>5}  {} {}   {:.0} s ago, {}",
                        r.port, r.address, r.args, r.age, r.from
                    ))
                    .monospace()
                    .color(weak),
                );
            }
        }
    }

    fn audio(&mut self, ui: &mut Ui, engine: &Arc<Engine>, auto: &mut automation::Registry) {
        ui.heading("Audio");
        let weak = theme::TEXT_WEAK;
        let hub = engine.audio();
        let mut config = hub.config();
        let r = ui.checkbox(&mut config.enabled, "listen").on_hover_text(
            "Off closes every audio input for this run (audio.config enabled); the nodes stay",
        );
        auto.add("helpers.audio.enabled", r.rect, "listen");
        if r.changed() {
            hub.set_config(config);
        }
        let streams = hub.status();
        if streams.is_empty() {
            ui.label(RichText::new("Nothing listens: add a chan.audioin node (Tab > chan).").color(weak));
        }
        for s in &streams {
            let asked =
                if s.key.is_empty() { "the default input".to_string() } else { format!("`{}`", s.key) };
            let mut text = s.device.clone().unwrap_or_else(|| asked.clone());
            if s.rate > 0 {
                text.push_str(&format!(" \u{b7} {} Hz \u{b7} {} channels", s.rate, s.channels));
            }
            if s.glitches > 0 {
                text.push_str(&format!(" \u{b7} {} glitches", s.glitches));
            }
            ui.label(text).on_hover_text(format!(
                "asked for: {asked}; opened {} times; {} frames received",
                s.opens, s.frames
            ));
            if let Some(e) = &s.error {
                ui.colored_label(if s.open { weak } else { theme::WARNING }, e);
            }
            if s.open {
                for (i, db) in s.peak_db.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("ch{}", i + 1)).monospace().color(weak));
                        meter(ui, *db);
                        ui.label(RichText::new(format!("{db:>6.1} dB")).monospace().color(weak));
                    });
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("inputs on this machine").color(weak));
            let r = ui.small_button("list again");
            auto.add("helpers.audio.list", r.rect, "list again");
            if r.clicked() || self.audio_inputs.is_none() {
                self.audio_inputs = Some(penumbra_audio::capture::devices());
            }
        });
        match &self.audio_inputs {
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("none").color(weak));
            }
            Some(Ok(list)) => {
                for d in list {
                    let mut text = d.name.clone();
                    if let (Some(r), Some(c)) = (d.rate, d.channels) {
                        text.push_str(&format!("   {r} Hz, {c} ch"));
                    }
                    if d.default {
                        text.push_str("   (default)");
                    }
                    ui.label(RichText::new(text).monospace().color(weak))
                        .on_hover_text("Put part of this name into a chan.audioin's device");
                }
            }
            Some(Err(e)) => {
                ui.colored_label(theme::WARNING, e);
            }
            None => {}
        }
    }

    fn machine(&mut self, ui: &mut Ui, auto: &mut automation::Registry) {
        ui.heading("Show machine");
        let checks = self.checks(Instant::now(), RECHECK).to_vec();
        if checks.is_empty() {
            ui.label(RichText::new("No checks on this platform yet.").color(theme::TEXT_WEAK));
        }
        let mut changed = false;
        for c in &checks {
            ui.horizontal(|ui| {
                let (color, word) = match c.status {
                    CheckStatus::Ok => (theme::TEXT_WEAK, "ok"),
                    CheckStatus::Problem => (theme::WARNING, "fix"),
                    CheckStatus::Unknown => (theme::ERROR, "?"),
                };
                ui.label(RichText::new(c.title).strong()).on_hover_text(c.explanation);
                ui.colored_label(color, format!("{word}: {}", c.detail)).on_hover_text(c.values.join("\n"));
                if c.status == CheckStatus::Problem {
                    let r = ui.button("Fix").on_hover_text(c.explanation);
                    auto.add(format!("helpers.fix.{}", c.id), r.rect, "Fix");
                    if r.clicked() {
                        match penumbra_system::checks::fix(c.id) {
                            Ok(previous) => {
                                self.previous.insert(c.id.to_string(), previous);
                                self.message = None;
                            }
                            Err(e) => self.message = Some(e.0),
                        }
                        changed = true;
                    }
                }
                if let Some(previous) = self.previous.get(c.id) {
                    let r = ui.button("Restore").on_hover_text("Put back what the fix replaced");
                    auto.add(format!("helpers.restore.{}", c.id), r.rect, "Restore");
                    if r.clicked() {
                        match penumbra_system::checks::restore(previous) {
                            Ok(()) => {
                                self.previous.remove(c.id);
                                self.message = None;
                            }
                            Err(e) => self.message = Some(e.0),
                        }
                        changed = true;
                    }
                }
            });
        }
        if let Some(m) = &self.message {
            ui.colored_label(theme::ERROR, m);
        }
        if changed {
            self.checks = None;
        }
    }
}

/// The resolution analysis: every node a projector shows, its size against what the projectors
/// need, the overlay switch and the optimise button.
fn resolution(
    ui: &mut Ui,
    input: &HelpersInput<'_>,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.heading("Resolution");
        let mut on = input.view.overlay;
        let r = ui.checkbox(&mut on, "overlay").on_hover_text(
            "Colour the mappers' and render nodes' pictures: green where a surface or texture pixel meets \
             about one projector pixel, red where it is too coarse, blue where it is finer than shown \
             (res.overlay)",
        );
        auto.add("helpers.overlay", r.rect, "overlay");
        if r.changed() {
            let session = input.engine.session().clone();
            input.engine.post_cook(move |st| {
                let net = session.doc().snapshot();
                st.set_overlay(on, &net, session.types());
            });
        }
    });
    let Some(report) = input.view.resolution.as_deref() else {
        ui.label(RichText::new("Measuring ...").color(theme::TEXT_WEAK));
        return;
    };
    ui.label(
        RichText::new(
            "What the projectors need of every node they show: pixels so that one lands on about one \
             projector pixel (95th percentile over each mapper's and render node's picture).",
        )
        .color(theme::TEXT_WEAK),
    );
    if !report.pending.is_empty() {
        ui.label(
            RichText::new(format!("Measuring what the camera sees: {}", report.pending.join(", ")))
                .color(theme::TEXT_WEAK),
        );
    }
    for p in report.projectors.iter().filter(|p| p.assumed) {
        ui.label(
            RichText::new(format!("{}: display unknown, {} x {} assumed", p.path, p.raster[0], p.raster[1]))
                .color(theme::TEXT_WEAK),
        );
    }
    if report.nodes.is_empty() {
        ui.label(RichText::new("No projector shows anything yet.").color(theme::TEXT_WEAK));
        return;
    }
    table(ui, report, input.net, auto, actions);
    let changes = report.changes(None);
    ui.horizontal(|ui| {
        let text = match changes.len() {
            0 => "Optimize".to_string(),
            1 => "Optimize 1 node".to_string(),
            n => format!("Optimize {n} nodes"),
        };
        let r = ui.add_enabled(input.can_edit && !changes.is_empty(), egui::Button::new(text)).on_hover_text(
            "Give these nodes the suggested width and height, one undo step. Nodes with keep_size on \
                 are left alone. Params in pixels (a blur's size, a text's size) look different afterwards.",
        );
        auto.add("helpers.optimize", r.rect, "Optimize");
        if r.clicked() {
            let ops = report.optimize_ops(input.net, None);
            if !ops.is_empty() {
                actions.push(Action::Edit {
                    ops,
                    label: "optimize resolutions".into(),
                    group: None,
                    place: None,
                });
            }
        }
        let kept = report.nodes.iter().filter(|n| n.keep && n.suggested.is_some()).count();
        if kept > 0 {
            ui.label(RichText::new(format!("{kept} kept by keep_size")).color(theme::TEXT_WEAK));
        }
    });
}

fn table(
    ui: &mut Ui,
    report: &Report,
    net: &Network,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let size = |s: [u32; 2]| format!("{} x {}", s[0], s[1]);
    egui::Grid::new("resolution").striped(true).num_columns(5).show(ui, |ui| {
        for h in ["node", "size", "needs", "", "suggested"] {
            ui.label(RichText::new(h).color(theme::TEXT_WEAK));
        }
        ui.end_row();
        for n in &report.nodes {
            let r = ui.add(egui::Label::new(RichText::new(&n.path).monospace()).sense(egui::Sense::click()));
            let r = r.on_hover_text(format!("{} (click to select)", n.kind));
            auto.add(format!("helpers.res.{}", n.path), r.rect, &n.path);
            if r.clicked()
                && let Ok(path) = n.path.parse()
                && let Some(id) = net.resolve(&path)
            {
                actions.push(Action::SelectNodes([id].into()));
            }
            ui.label(n.size.map_or("-".to_string(), size));
            ui.label(size(n.needed));
            let ratio = n.ratio.map_or(String::new(), |r| format!("{r:.2} "));
            match n.verdict {
                Verdict::Ok => ui.label(RichText::new(format!("{ratio}ok")).color(theme::TEXT_WEAK)),
                Verdict::Coarse => ui.colored_label(theme::ERROR, format!("{ratio}too coarse")),
                Verdict::Wasted => ui.colored_label(theme::ACCENT, format!("{ratio}wasted")),
                Verdict::Unknown => ui.label(RichText::new("not cooked").color(theme::TEXT_WEAK)),
            };
            match (n.suggested, n.sets_size, n.keep) {
                (Some(s), _, true) => {
                    ui.label(RichText::new(format!("{} (kept)", size(s))).color(theme::TEXT_WEAK))
                }
                (Some(s), _, false) => ui.label(size(s)),
                (None, false, _) if n.verdict != Verdict::Ok => {
                    ui.label(RichText::new("follows its input").color(theme::TEXT_WEAK))
                }
                _ => ui.label(""),
            };
            ui.end_row();
        }
    });
}

/// Runs `work` on a thread of its own (a save or restore waits for files and the GPU, which the
/// UI must not).
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(work) {
        log::error!("cannot start {name}: {e}");
    }
}

/// A level meter from -60 to 0 dBFS; red above -1 dBFS, where clipping is near.
fn meter(ui: &mut Ui, db: f64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(180.0, 10.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG_USER);
    // `as`: a fraction of the width.
    let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f32;
    let mut fill = rect;
    fill.set_width(rect.width() * t);
    let color = if db > -1.0 { theme::ERROR } else { theme::kind_color(penumbra_graph::DataKind::Chan) };
    painter.rect_filled(fill, 2.0, color);
}
