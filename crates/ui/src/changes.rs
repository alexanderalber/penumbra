//! The change log: the history entries with who made them (you, Claude, the project file), what
//! they touched and when. Hovering an entry outlines its nodes in the editor, clicking selects
//! them. Undo stays linear.

use std::collections::{BTreeSet, HashMap};
use std::time::{Instant, SystemTime};

use egui::{RichText, Ui};
use penumbra_core::NodeId;
use penumbra_graph::{Document, Source};

use crate::icons::{self, Icon};
use crate::{Action, automation, theme};

/// Entries shown; older ones stay undoable, just not listed.
const SHOWN: usize = 200;

#[derive(Clone)]
struct Row {
    index: usize,
    source: Source,
    label: String,
    touched: BTreeSet<NodeId>,
    at: SystemTime,
    applied: bool,
}

#[derive(Default)]
pub struct Changes {
    rows: Vec<Row>,
    /// (entries, applied, time of the newest entry): the rows are rebuilt when it changes.
    signature: Option<(usize, usize, Option<SystemTime>)>,
    /// Nodes of the entry under the pointer.
    pub hovered: BTreeSet<NodeId>,
}

pub fn source_name(source: Source) -> &'static str {
    match source {
        Source::Ui => "you",
        Source::ControlChannel => "Claude",
        Source::ProjectFile => "file",
        Source::Osc => "OSC",
    }
}

fn source_color(source: Source) -> egui::Color32 {
    match source {
        Source::Ui => theme::TEXT_WEAK,
        Source::ControlChannel => theme::CLAUDE,
        Source::ProjectFile => theme::FILE,
        Source::Osc => theme::TEXT,
    }
}

impl Changes {
    /// Re-reads the history if it changed.
    pub fn refresh(&mut self, doc: &Document) {
        let signature =
            doc.with_history(|h| (h.entries().len(), h.applied(), h.entries().last().map(|e| e.at)));
        if self.signature == Some(signature) {
            return;
        }
        self.signature = Some(signature);
        self.rows = doc.with_history(|h| {
            let applied = h.applied();
            h.entries()
                .iter()
                .enumerate()
                .rev()
                .take(SHOWN)
                .map(|(index, e)| Row {
                    index,
                    source: e.source,
                    label: e.label.clone(),
                    touched: e.touched.clone(),
                    at: e.at,
                    applied: index < applied,
                })
                .collect()
        });
    }

    /// Nodes touched by applied edits from Claude or the file in the last few seconds, with
    /// when, for the editor's fading outline.
    pub fn recent(&self, now: Instant) -> HashMap<NodeId, Instant> {
        let wall = SystemTime::now();
        let mut out = HashMap::new();
        for row in self.rows.iter().filter(|r| r.applied && r.source != Source::Ui) {
            let age = wall.duration_since(row.at).unwrap_or_default();
            if age.as_secs_f32() > theme::RECENT_SECONDS {
                break;
            }
            let at = now.checked_sub(age).unwrap_or(now);
            for id in &row.touched {
                out.entry(*id).or_insert(at);
            }
        }
        out
    }

    /// The entries, newest first.
    pub fn show(&mut self, ui: &mut Ui, auto: &mut automation::Registry, actions: &mut Vec<Action>) {
        self.hovered.clear();
        ui.separator();
        let wall = SystemTime::now();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.rows.is_empty() {
                ui.label(RichText::new("no changes yet").color(theme::TEXT_WEAK));
            }
            for row in &self.rows {
                let age = wall.duration_since(row.at).unwrap_or_default().as_secs();
                let label = if row.label.is_empty() { "edit" } else { row.label.as_str() };
                let nodes = match row.touched.len() {
                    1 => "1 node".to_string(),
                    n => format!("{n} nodes"),
                };
                let mut text = RichText::new(format!("{label}  \u{b7} {nodes} \u{b7} {}", ago(age)));
                let mut who = RichText::new(source_name(row.source)).color(source_color(row.source)).strong();
                if !row.applied {
                    text = text.color(theme::TEXT_WEAK).strikethrough();
                    who = who.weak();
                }
                let r = ui
                    .horizontal(|ui| {
                        ui.label(who);
                        ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click()))
                    })
                    .inner;
                auto.add(format!("history.{}", row.index), r.rect, label);
                if r.hovered() {
                    self.hovered.clone_from(&row.touched);
                }
                if r.clicked() {
                    actions.push(Action::SelectNodes(row.touched.clone()));
                }
            }
        });
    }
}

/// How far the header's buttons sit below the row's centre: centred, their labels looked higher
/// than the section title next to them.
const BUTTONS_DROP: f32 = 2.0;

/// Undo and Redo, for the section header (usable while the list is collapsed).
pub fn buttons(
    ui: &mut Ui,
    can_undo: bool,
    can_redo: bool,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let rect = ui.available_rect_before_wrap().translate(egui::vec2(0.0, BUTTONS_DROP));
    let layout = *ui.layout();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        let r = ui.add_enabled_ui(can_undo, |ui| icons::button(ui, Icon::Undo, "Undo")).inner;
        auto.add("button.undo", r.rect, "Undo");
        if r.on_hover_text("Ctrl+Z").clicked() {
            actions.push(Action::Undo);
        }
        let r = ui.add_enabled_ui(can_redo, |ui| icons::button(ui, Icon::Redo, "Redo")).inner;
        auto.add("button.redo", r.rect, "Redo");
        if r.on_hover_text("Ctrl+Shift+Z").clicked() {
            actions.push(Action::Redo);
        }
    });
}

/// Minutes at the finest: a seconds count would tick every second.
fn ago(seconds: u64) -> String {
    match seconds {
        0..=9 => "now".into(),
        10..=59 => "<1 min ago".into(),
        60..=3599 => format!("{} min ago", seconds / 60),
        _ => format!("{} h ago", seconds / 3600),
    }
}
