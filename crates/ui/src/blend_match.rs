//! The Helpers window's "Blend and match" section: blend weights from the scans in the project,
//! the colour match of the projectors and their flat field, each with its few options, the job
//! going on with a cancel button, and how the last of each ended. The same things are
//! `scan.folders`, `scan.blend`, `projector.match`, `projector.flatfield`, `job.status` and
//! `job.cancel` on the control channel; the buttons start those commands' jobs and leave the
//! files where the commands put them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use egui::{RichText, Ui};
use penumbra_core::{NodePath, Value};
use penumbra_engine::Engine;
use penumbra_engine::flatfield::{Even, FlatfieldArgs};
use penumbra_engine::jobs::{Finished, Running};
use penumbra_engine::projector::MatchArgs;
use penumbra_engine::projector_stored::Source;
use penumbra_engine::scan::BlendArgs;
use penumbra_engine::scan_run::ScanFolder;
use penumbra_graph::Network;
use penumbra_project::Actor;
use serde_json::Value as Json;

use crate::helpers::HelpersInput;
use crate::{automation, theme};

/// The command names, as the job slot knows them.
const BLEND: &str = "scan.blend";
const MATCH: &str = "projector.match";
const FLATFIELD: &str = "projector.flatfield";

/// What the section remembers between frames: the scans found and the options chosen.
pub struct BlendMatch {
    /// The scans in the project; looked up again after a job ended and on request.
    scans: Option<Vec<ScanFolder>>,
    /// The end time of the newest finished job seen, so the scans are looked up after the next.
    seen: f64,
    /// Scans left out of the blend (all others take part).
    left_out: BTreeMap<String, bool>,
    ramp: f32,
    into_scans: Option<bool>,
    out: String,
    camera: String,
    /// Projectors left out of the match and the flat field.
    skipped: BTreeMap<String, bool>,
    keep: bool,
    save: String,
    note: String,
    apply: bool,
    even: Even,
    /// The scan chosen per projector for the flat field, where it differs from the one found.
    flat_scans: BTreeMap<String, String>,
    /// Why the last start failed.
    message: Option<String>,
}

impl Default for BlendMatch {
    fn default() -> Self {
        Self {
            scans: None,
            seen: 0.0,
            left_out: BTreeMap::new(),
            ramp: 200.0,
            into_scans: None,
            out: "objects".into(),
            camera: String::new(),
            skipped: BTreeMap::new(),
            keep: false,
            save: "measured".into(),
            note: String::new(),
            apply: true,
            even: Even::All,
            flat_scans: BTreeMap::new(),
            message: None,
        }
    }
}

/// The paths of the nodes of `kind`, those switched off (`active` false) left out.
fn paths_of(input: &HelpersInput<'_>, kind: &str) -> Vec<String> {
    let desc = input.engine.types().desc(kind);
    input
        .net
        .nodes()
        .filter(|n| n.kind == kind)
        .filter(|n| !matches!(desc.and_then(|d| n.param(d, "active")), Some(Value::Bool(false))))
        .filter_map(|n| input.net.path_of(n.id).map(|p| p.to_string()))
        .collect()
}

/// The camera the `tex.scan` nodes name, as a path.
fn scan_camera(engine: &Engine, net: &Network) -> Option<String> {
    use penumbra_ops::tex::scan::SCAN;
    let desc = engine.types().desc(SCAN)?;
    net.nodes().filter(|n| n.kind == SCAN).find_map(|n| {
        let text = n.param(desc, "camera").and_then(Value::as_str)?.trim();
        let id = net.resolve_from(n.id, text).ok()?;
        net.path_of(id).map(|p| p.to_string())
    })
}

fn ago(unix_time: f64) -> String {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let s = (now - unix_time).max(0.0);
    if s < 90.0 {
        format!("{s:.0} s ago")
    } else if s < 5400.0 {
        format!("{:.0} min ago", s / 60.0)
    } else if s < 129_600.0 {
        format!("{:.0} h ago", s / 3600.0)
    } else {
        format!("{:.0} days ago", s / 86_400.0)
    }
}

fn parse(path: &str) -> Option<NodePath> {
    NodePath::parse(path).ok()
}

impl BlendMatch {
    pub fn show(&mut self, ui: &mut Ui, input: &HelpersInput<'_>, auto: &mut automation::Registry) {
        let engine = input.engine;
        let status = engine.jobs().status();
        let newest = status.last.values().map(|f| f.unix_time).fold(0.0, f64::max);
        if self.scans.is_none() || newest > self.seen {
            self.scans = Some(penumbra_engine::scan_run::scan_folders(engine));
            self.seen = newest;
        }
        ui.heading("Blend and match");
        ui.label(
            RichText::new(
                "Blend weights from the scans, then the colour match and the even light of the \
                 projectors, measured with the camera.",
            )
            .color(theme::TEXT_WEAK),
        );
        let busy = status.running.is_some();
        if let Some(r) = &status.running {
            running(ui, r, engine, auto);
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if let Some(m) = &self.message {
            ui.colored_label(theme::ERROR, m);
        }
        let can = input.can_edit && !busy;
        let why =
            if busy { "Another job is running" } else { "Not while Claude is editing or show mode is on" };

        ui.add_space(4.0);
        self.blend(ui, input, can, why, auto);
        finished(ui, status.last.get(BLEND), auto);

        ui.add_space(4.0);
        let projectors = self.rig(ui, input, auto);
        self.matching(ui, input, &projectors, can, why, auto);
        finished(ui, status.last.get(MATCH), auto);

        ui.add_space(4.0);
        self.flatfield(ui, input, &projectors, can, why, auto);
        finished(ui, status.last.get(FLATFIELD), auto);
    }

    fn start(&mut self, outcome: Result<Json, penumbra_engine::CommandError>) {
        self.message = outcome.err().map(|e| e.0);
    }

    /// The scans and the blend's options and button.
    fn blend(
        &mut self,
        ui: &mut Ui,
        input: &HelpersInput<'_>,
        can: bool,
        why: &str,
        auto: &mut automation::Registry,
    ) {
        let weak = theme::TEXT_WEAK;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Scans").strong()).on_hover_text(
                "Folders in the project with a scan's maps (cam_to_proj.exr, proj_to_cam.exr): a tex.scan's \
                 folder, or where scan.capture or scan.decode wrote (scan.folders)",
            );
            let r = ui.small_button("look again");
            auto.add("helpers.scans.refresh", r.rect, "look again");
            if r.clicked() {
                self.scans = None;
            }
        });
        let scans = self.scans.clone().unwrap_or_default();
        if scans.is_empty() {
            ui.label(
                RichText::new("No scans in the project yet: scan with a tex.scan node (its scan button).")
                    .color(weak),
            );
            return;
        }
        for s in &scans {
            ui.horizontal(|ui| {
                let mut on = !self.left_out.get(&s.folder).copied().unwrap_or(false);
                let r = ui.checkbox(&mut on, RichText::new(&s.folder).monospace());
                auto.add(format!("helpers.scan.{}", s.folder), r.rect, &s.folder);
                if r.changed() {
                    self.left_out.insert(s.folder.clone(), !on);
                }
                let mut about = Vec::new();
                if let Some(p) = &s.projector {
                    about.push(p.clone());
                }
                if s.blended {
                    about.push("blended".into());
                }
                if let Some(t) = s.modified {
                    about.push(format!("scanned {}", ago(t)));
                }
                let r = ui.label(RichText::new(about.join(" \u{b7} ")).color(weak));
                if let Some(n) = &s.node {
                    r.on_hover_text(format!("written by {n}"));
                }
            });
        }
        let chosen: Vec<String> = scans
            .iter()
            .filter(|s| !self.left_out.get(&s.folder).copied().unwrap_or(false))
            .map(|s| s.folder.clone())
            .collect();
        let into = self.into_scans.unwrap_or_else(|| scans.iter().any(|s| s.node.is_some()));
        ui.horizontal(|ui| {
            ui.label("ramp").on_hover_text(
                "How wide the soft edge runs from where another projector starts, in projector pixels",
            );
            let r = ui.add(egui::DragValue::new(&mut self.ramp).range(1.0..=2000.0).suffix(" px").speed(2.0));
            auto.add("helpers.blend.ramp", r.rect, "ramp");
            let mut into_now = into;
            let r = ui.checkbox(&mut into_now, "into the scan folders").on_hover_text(
                "weight.png, detail.png and proj_to_cam_smooth.exr go into each scan's folder, where its \
                 tex.scan reads them. Off: numbered files (weight_0.png, ... in the order listed) into the \
                 folder named beside it",
            );
            auto.add("helpers.blend.into_scans", r.rect, "into the scan folders");
            if r.changed() {
                self.into_scans = Some(into_now);
            }
            if !into_now {
                let r = ui.add(egui::TextEdit::singleline(&mut self.out).desired_width(90.0));
                auto.add("helpers.blend.out", r.rect, "out");
            }
            let tip = "Blend weights for the chosen scans: where projectors overlap, each one fades out \
                       towards its edge so the light adds up evenly (scan.blend)";
            let ok = can && !chosen.is_empty();
            let r = ui
                .add_enabled(ok, egui::Button::new("Blend"))
                .on_hover_text(tip)
                .on_disabled_hover_text(if chosen.is_empty() { "Choose a scan" } else { why });
            auto.add("helpers.blend.run", r.rect, "Blend");
            if r.clicked() {
                let a = BlendArgs {
                    scans: chosen.iter().map(PathBuf::from).collect(),
                    out: (!into_now && !self.out.trim().is_empty()).then(|| PathBuf::from(self.out.trim())),
                    ramp: Some(self.ramp),
                    into_scans: Some(into_now),
                    wait: Some(false),
                    ..BlendArgs::default()
                };
                self.start(penumbra_engine::scan::blend_job(input.engine, a, Actor::User));
            }
        });
    }

    /// The camera and the projectors both measurements use; the projectors taking part.
    fn rig(&mut self, ui: &mut Ui, input: &HelpersInput<'_>, auto: &mut automation::Registry) -> Vec<String> {
        use penumbra_ops::out::PROJECTOR;
        use penumbra_ops::tex::video_in::VIDEO_IN;
        let cameras = paths_of(input, VIDEO_IN);
        if !cameras.contains(&self.camera) {
            self.camera = scan_camera(input.engine, input.net)
                .filter(|c| cameras.contains(c))
                .or_else(|| cameras.first().cloned())
                .unwrap_or_default();
        }
        let all = paths_of(input, PROJECTOR);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Camera").strong())
                .on_hover_text("The tex.video_in that sees what the projectors light");
            if cameras.is_empty() {
                ui.colored_label(theme::WARNING, "none: add a tex.video_in");
            } else {
                let r = egui::ComboBox::from_id_salt("helpers.match.camera")
                    .selected_text(RichText::new(&self.camera).monospace())
                    .show_ui(ui, |ui| {
                        for c in &cameras {
                            ui.selectable_value(&mut self.camera, c.clone(), RichText::new(c).monospace());
                        }
                    });
                auto.add("helpers.match.camera", r.response.rect, "camera");
            }
            ui.label(RichText::new("Projectors").strong());
            if all.is_empty() {
                ui.colored_label(theme::WARNING, "no active out.projector");
            }
            for p in &all {
                let mut on = !self.skipped.get(p).copied().unwrap_or(false);
                let r = ui.checkbox(&mut on, RichText::new(p).monospace());
                auto.add(format!("helpers.match.projector.{p}"), r.rect, p);
                if r.changed() {
                    self.skipped.insert(p.clone(), !on);
                }
            }
        });
        all.into_iter().filter(|p| !self.skipped.get(p).copied().unwrap_or(false)).collect()
    }

    fn matching(
        &mut self,
        ui: &mut Ui,
        input: &HelpersInput<'_>,
        projectors: &[String],
        can: bool,
        why: &str,
        auto: &mut automation::Registry,
    ) {
        ui.label(RichText::new("Colour match").strong()).on_hover_text(
            "Projectors of different models or age differ in brightness, white and tone curve; the match \
             makes them show the same greys and white, as bright as the weakest allows",
        );
        ui.horizontal_wrapped(|ui| {
            let r = ui.checkbox(&mut self.keep, "keep the measurements in").on_hover_text(
                "Also measures a grey ramp and writes one file per projector (NAME.json, after the node's \
                 name) into this folder, to match from later without the camera",
            );
            auto.add("helpers.match.keep", r.rect, "keep the measurements");
            let r = ui.add(egui::TextEdit::singleline(&mut self.save).desired_width(90.0));
            auto.add("helpers.match.folder", r.rect, "folder");
            ui.label("note").on_hover_text("Kept in the files: picture mode, lamp hours, where they stood");
            let r = ui.add(egui::TextEdit::singleline(&mut self.note).desired_width(140.0));
            auto.add("helpers.match.note", r.rect, "note");
        });
        ui.horizontal(|ui| {
            let r = ui.checkbox(&mut self.apply, "apply").on_hover_text(
                "Write the result into the projectors' colour params (matrix, curves; flat field: the gain and \
                 black inputs) as one undo step. Off: only measure and report",
            );
            auto.add("helpers.match.apply", r.rect, "apply");
            let ready = can && !projectors.is_empty() && !self.camera.is_empty();
            let need = if projectors.is_empty() {
                "Choose a projector"
            } else if self.camera.is_empty() {
                "Add a tex.video_in that sees the projectors"
            } else {
                why
            };
            let r = ui
                .add_enabled(ready, egui::Button::new("Match"))
                .on_hover_text(
                    "Measures each projector with the camera (about 25 s each; nobody in the picture) and \
                     bends the others to show the same greys and white as the reference (projector.match)",
                )
                .on_disabled_hover_text(need);
            auto.add("helpers.match.run", r.rect, "Match");
            if r.clicked() {
                let save = self.keep && !self.save.trim().is_empty();
                let a = MatchArgs {
                    camera: parse(&self.camera),
                    projectors: Some(projectors.iter().filter_map(|p| parse(p)).collect()),
                    save: save.then(|| self.save.trim().to_string()),
                    note: (save && !self.note.trim().is_empty()).then(|| self.note.trim().to_string()),
                    apply: Some(self.apply),
                    wait: Some(false),
                    ..MatchArgs::default()
                };
                self.start(penumbra_engine::projector::match_job(input.engine, a, Actor::User));
            }
            let folder = self.save.trim();
            let stored = !folder.is_empty() && input.engine.session().dir().join(folder).is_dir();
            let r = ui
                .add_enabled(can && stored && !projectors.is_empty(), egui::Button::new("Match from the files"))
                .on_hover_text(
                    "Matches from the measurements kept in the folder (one NAME.json per projector), without \
                     the camera: open loop, trust it less than a camera run",
                )
                .on_disabled_hover_text(if stored { need } else { "No such folder: keep measurements first" });
            auto.add("helpers.match.from", r.rect, "Match from the files");
            if r.clicked() {
                let a = MatchArgs {
                    from: Some(Source::Folder(folder.to_string())),
                    projectors: Some(projectors.iter().filter_map(|p| parse(p)).collect()),
                    apply: Some(self.apply),
                    wait: Some(false),
                    ..MatchArgs::default()
                };
                self.start(penumbra_engine::projector::match_job(input.engine, a, Actor::User));
            }
        });
    }

    fn flatfield(
        &mut self,
        ui: &mut Ui,
        input: &HelpersInput<'_>,
        projectors: &[String],
        can: bool,
        why: &str,
        auto: &mut automation::Registry,
    ) {
        let scans = self.scans.clone().unwrap_or_default();
        let mut chosen: Vec<(String, Option<String>)> = Vec::new();
        ui.label(RichText::new("Flat field").strong()).on_hover_text(
            "Measures how brightness and colour vary across each projector's picture and the brighter black \
             in overlaps; writes a gain map and a black lift per projector into flatfield/ and connects \
             them (projector.flatfield). Each projector needs a scan that fits the setup now",
        );
        egui::Grid::new("helpers.flatfield.scans").num_columns(2).show(ui, |ui| {
            for p in projectors {
                ui.label(RichText::new(p).monospace());
                let found =
                    scans.iter().find(|s| s.projector.as_deref() == Some(p)).map(|s| s.folder.clone());
                let mut pick = self.flat_scans.get(p).cloned().or(found).unwrap_or_default();
                let before = pick.clone();
                let r = egui::ComboBox::from_id_salt(format!("helpers.flatfield.scan.{p}"))
                    .selected_text(if pick.is_empty() { "choose its scan" } else { pick.as_str() })
                    .show_ui(ui, |ui| {
                        for s in &scans {
                            ui.selectable_value(
                                &mut pick,
                                s.folder.clone(),
                                RichText::new(&s.folder).monospace(),
                            );
                        }
                    });
                auto.add(format!("helpers.flatfield.scan.{p}"), r.response.rect, "scan");
                if pick != before {
                    self.flat_scans.insert(p.clone(), pick.clone());
                }
                chosen.push((p.clone(), (!pick.is_empty()).then_some(pick)));
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.label("even");
            for (value, label, tip) in [
                (Even::All, "brightness and colour", "On a flat, evenly coloured surface (the bare wall)"),
                (
                    Even::Colour,
                    "colour only",
                    "On a relief or objects: their shading changes the brightness, not the colour",
                ),
            ] {
                let r = ui.radio_value(&mut self.even, value, label).on_hover_text(tip);
                auto.add(format!("helpers.flatfield.even.{label}"), r.rect, label);
            }
            let missing = chosen.iter().any(|(_, s)| s.is_none());
            let ready = can && !projectors.is_empty() && !self.camera.is_empty() && !missing;
            let need = if missing {
                "Choose a scan for every projector"
            } else if projectors.is_empty() || self.camera.is_empty() {
                "Choose a camera and a projector"
            } else {
                why
            };
            let r = ui
                .add_enabled(ready, egui::Button::new("Measure"))
                .on_hover_text("About 20 s per projector; nobody in the picture (projector.flatfield)")
                .on_disabled_hover_text(need);
            auto.add("helpers.flatfield.run", r.rect, "Measure");
            if r.clicked() {
                let a = FlatfieldArgs {
                    camera: parse(&self.camera).unwrap_or_default(),
                    projectors: chosen.iter().filter_map(|(p, _)| parse(p)).collect(),
                    scans: chosen.iter().filter_map(|(_, s)| s.as_ref().map(PathBuf::from)).collect(),
                    apply: Some(self.apply),
                    even: self.even,
                    wait: Some(false),
                    ..FlatfieldArgs::default()
                };
                self.start(penumbra_engine::flatfield::flatfield_job(input.engine, a, Actor::User));
            }
        });
    }
}

/// The job going on: a progress bar with its step, the time, a cancel button.
fn running(ui: &mut Ui, r: &Running, engine: &Arc<Engine>, auto: &mut automation::Registry) {
    ui.horizontal(|ui| {
        let b = ui
            .button("Cancel")
            .on_hover_text("Stop before the next picture; nothing is applied (job.cancel)");
        auto.add("helpers.job.cancel", b.rect, "Cancel");
        if b.clicked() {
            engine.jobs().cancel();
        }
        let by = if r.by == "claude" { ", started by Claude" } else { "" };
        let text = format!("{}: {} ({:.0} s{by})", r.command, r.step, r.seconds);
        let bar = egui::ProgressBar::new(r.progress.unwrap_or(0.0))
            .text(RichText::new(text).small())
            .desired_width(ui.available_width())
            .animate(r.progress.is_none());
        let p = ui.add(bar);
        auto.add("helpers.job.progress", p.rect, "progress");
    });
}

/// How the last job of a command ended, with what matters of its reply.
fn finished(ui: &mut Ui, f: Option<&Finished>, auto: &mut automation::Registry) {
    let Some(f) = f else { return };
    let weak = theme::TEXT_WEAK;
    let colour = if f.ok {
        weak
    } else if f.cancelled {
        theme::WARNING
    } else {
        theme::ERROR
    };
    let text = if f.cancelled { "cancelled".to_string() } else { f.message.clone() };
    let r =
        ui.colored_label(colour, format!("{} ({}, {:.0} s): {text}", f.command, ago(f.unix_time), f.seconds));
    auto.add(format!("helpers.last.{}", f.command), r.rect, &f.command);
    if f.ok {
        match f.command.as_str() {
            MATCH => match_result(ui, &f.reply),
            FLATFIELD => flatfield_result(ui, &f.reply),
            BLEND => blend_result(ui, &f.reply),
            _ => {}
        }
    }
    for w in &f.warnings {
        ui.colored_label(theme::WARNING, w);
    }
}

fn number(v: &Json) -> Option<f64> {
    v.as_f64()
}

/// The largest of a per-channel list, by size.
fn largest(v: &Json) -> Option<f64> {
    v.as_array()?.iter().filter_map(number).map(f64::abs).reduce(f64::max)
}

/// The match: per projector the light it keeps and, after a camera run, how far each test
/// colour still reads from the reference's in camera codes.
fn match_result(ui: &mut Ui, reply: &Json) {
    let weak = theme::TEXT_WEAK;
    let Some(projectors) = reply.get("projectors").and_then(Json::as_array) else { return };
    let reference = reply.get("reference").and_then(Json::as_str);
    // The colours checked after refining, in a fixed order: white, the primaries, the greys.
    let mut columns: Vec<String> = Vec::new();
    for p in projectors {
        if let Some(check) = p.pointer("/refine/check").and_then(Json::as_object) {
            let mut greys: Vec<(f64, String)> = check
                .keys()
                .filter_map(|k| k.strip_prefix("grey ").and_then(|g| g.parse().ok()).map(|g| (g, k.clone())))
                .collect();
            greys.sort_by(|a, b| a.0.total_cmp(&b.0));
            columns = ["white", "red", "green", "blue"].iter().map(|s| (*s).to_string()).collect();
            columns.extend(greys.into_iter().map(|(_, k)| k));
            break;
        }
    }
    egui::Grid::new("helpers.match.result").striped(true).show(ui, |ui| {
        ui.label(RichText::new("projector").color(weak));
        ui.label(RichText::new("keeps").color(weak)).on_hover_text(
            "The share of its full white the projector keeps after the match (peak at strength 1)",
        );
        for c in &columns {
            ui.label(RichText::new(c).color(weak)).on_hover_text(
                "How far the camera reads this colour, shown through the new correction, from the reference \
                 showing it: the largest of red, green and blue, in 8-bit camera codes",
            );
        }
        ui.end_row();
        for p in projectors {
            let path = p.get("path").and_then(Json::as_str).unwrap_or("?");
            let name = if Some(path) == reference { format!("{path} (reference)") } else { path.to_string() };
            ui.label(RichText::new(name).monospace());
            match p.pointer("/peak/strength_1").and_then(Json::as_f64) {
                Some(k) => ui.label(format!("{:.0} %", k * 100.0)),
                None => ui.label(""),
            };
            for c in &columns {
                let codes = if c == "white" {
                    p.pointer("/refine/rounds")
                        .and_then(Json::as_array)
                        .and_then(|r| r.iter().filter_map(|r| largest(&r["residual_codes"])).reduce(f64::min))
                } else {
                    p.pointer(&format!("/refine/check/{c}/residual_codes")).and_then(largest)
                };
                // Saturated colours match only roughly; white and the greys are what the match aims at.
                let aimed = c == "white" || c.starts_with("grey");
                match codes {
                    Some(v) if v > 4.0 && aimed => ui.colored_label(theme::WARNING, format!("{v:.1}")),
                    Some(v) => ui.label(format!("{v:.1}")),
                    None => ui.label(RichText::new("-").color(weak)),
                };
            }
            ui.end_row();
        }
    });
    if let Some(files) = reply.pointer("/saved/files").and_then(Json::as_array) {
        ui.label(RichText::new(format!("measurements kept: {} file(s)", files.len())).color(weak));
        if let Some(check) = reply.pointer("/saved/stored_match_against_this").and_then(Json::as_array) {
            let parts: Vec<String> = check
                .iter()
                .filter_map(|c| {
                    let path = c.get("path")?.as_str()?;
                    let d = largest(c.get("largest_signal_difference")?)?;
                    Some(format!("{path} {d:.3}"))
                })
                .collect();
            if !parts.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "matching from these files would differ from this run by at most: {}",
                        parts.join(", ")
                    ))
                    .color(weak),
                )
                .on_hover_text("The largest signal difference over a few greys; small numbers: the files are good to match from");
            }
        }
    }
}

fn flatfield_result(ui: &mut Ui, reply: &Json) {
    let weak = theme::TEXT_WEAK;
    if let Some(scale) = reply.get("scale").and_then(Json::as_f64) {
        ui.label(
            RichText::new(format!(
                "the projectors keep {:.0} % of their light where they are matched",
                scale * 100.0
            ))
            .color(weak),
        );
    }
    if let Some(projectors) = reply.get("projectors").and_then(Json::as_array) {
        for p in projectors {
            if let Some(gain) = p.pointer("/files/gain").and_then(Json::as_str) {
                let name = std::path::Path::new(gain).file_name().map(|n| n.to_string_lossy().to_string());
                ui.label(RichText::new(format!("wrote {}", name.unwrap_or_default())).color(weak))
                    .on_hover_text(gain);
            }
        }
    }
}

fn blend_result(ui: &mut Ui, reply: &Json) {
    let weak = theme::TEXT_WEAK;
    if let Some(projectors) = reply.get("projectors").and_then(Json::as_array) {
        for p in projectors {
            let scan = p.get("scan").and_then(Json::as_str).unwrap_or("?");
            let weight = p.get("weight").and_then(Json::as_str).unwrap_or("");
            let shared = p.get("shared").and_then(Json::as_u64).unwrap_or(0);
            ui.label(RichText::new(format!("{scan}: shares {shared} camera px with the others")).color(weak))
                .on_hover_text(format!("wrote {weight}"));
        }
    }
}
