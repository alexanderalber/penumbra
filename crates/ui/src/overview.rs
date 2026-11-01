//! The viewer's view of a `chan.beat_grid`: the overview of its sound file
//! ([`penumbra_ops::audio::overview`]: waveform, bars, sections, playhead), painted with egui.
//! The node's `overview` output draws the same shapes into a texture.
//!
//! A click or a drag in either lane moves the `audio.file` there (`audio.seek`). While that
//! node's `position` is bound to a channel, the channel decides where it is: the view says so
//! instead of seeking.

use std::path::Path;

use egui::{Rect, Sense, Ui};
use penumbra_core::NodeId;
use penumbra_graph::Network;
use penumbra_ops::audio::beat_grid::{self, BEAT_GRID, BeatFiles};
use penumbra_ops::audio::file::SoundFiles;
use penumbra_ops::audio::overview::{self, ColumnCache};
use penumbra_ops::meters;

use crate::meters::{EguiMeasure, paint};
use crate::{Action, automation, theme};

/// Where the viewer reads a sound file's analysis and playhead; the engine's, cheap to clone.
#[derive(Clone)]
pub struct BeatSources {
    pub beat_files: BeatFiles,
    pub sound_files: SoundFiles,
}

/// A `chan.beat_grid` as the viewer's header found it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridNode {
    pub id: NodeId,
    /// The `audio.file` wired into it.
    pub source: Option<NodeId>,
    /// That node's `position` is bound to a channel: no seeking.
    pub bound: bool,
    /// The `span` param: seconds the lower lane shows.
    pub span: f64,
}

impl GridNode {
    /// The grid node `id` of `net`, if it is one.
    pub fn of(net: &Network, id: NodeId) -> Option<GridNode> {
        let node = net.node(id)?;
        if node.kind != BEAT_GRID {
            return None;
        }
        let source = node.inputs.first().copied().flatten().map(|p| p.node);
        let bound = source.and_then(|s| net.node(s)).is_some_and(|n| n.is_bound("position"));
        let span = node.params.get("span").and_then(|v| v.as_f64()).unwrap_or(overview::DEFAULT_SPAN);
        Some(GridNode { id, source, bound, span })
    }
}

/// The view's memory: the upper lane's columns, and the last seek a drag asked for.
#[derive(Default)]
pub struct OverviewView {
    node: Option<NodeId>,
    columns: ColumnCache,
    last_seek: Option<f64>,
}

impl OverviewView {
    /// Draws the overview of `node` into `rect` and turns clicks into seeks.
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        node: &GridNode,
        sources: Option<&BeatSources>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if self.node != Some(node.id) {
            *self = OverviewView { node: Some(node.id), ..OverviewView::default() };
        }
        let painter = ui.painter_at(rect);
        let room = meters::Rect::from_min_max([rect.min.x, rect.min.y], [rect.max.x, rect.max.y]);
        let measure = EguiMeasure(&painter);
        let palette = theme::meters_palette();
        let status = node.source.and_then(|s| sources.and_then(|b| b.sound_files.get(s)));
        let analysis = status
            .as_ref()
            .filter(|s| !s.file.is_empty())
            .and_then(|s| sources.and_then(|b| b.beat_files.get(Path::new(&s.file))));
        let state = analysis.map(|a| a.state());
        let line = match (&status, &state) {
            (None, _) => Some("wire the out of an audio.file into audio".to_string()),
            (Some(s), _) if s.file.is_empty() => Some("the audio.file has no file".to_string()),
            (Some(_), None) => Some("not analysed yet".to_string()),
            (Some(_), Some(st)) => beat_grid::status_line(st),
        };
        let o = beat_grid::overview_of(
            state.as_ref().and_then(|s| s.grid.as_deref()),
            state.as_ref().and_then(|s| s.envelopes.as_deref()),
            status.as_ref().map_or(0.0, |s| s.duration),
            status.as_ref().map(|s| s.in_file),
            node.span,
            line.as_deref(),
        );
        let (layout, shapes) =
            overview::shapes(room, &o, painter.pixels_per_point(), &mut self.columns, &palette, &measure);
        for shape in shapes {
            paint(&painter, shape);
        }

        let response = ui.interact(rect, ui.id().with(("overview", node.id)), Sense::click_and_drag());
        auto.add("viewer.overview", rect, "overview");
        let at = |p: egui::Pos2| layout.seconds_at([p.x, p.y]);
        if let Some(t) = response.hover_pos().and_then(at) {
            let tip = if node.bound {
                format!(
                    "{}: the audio.file's position is bound to a channel, so a click does not seek",
                    overview::clock(t)
                )
            } else {
                format!("{}: click to play from here", overview::clock(t))
            };
            response.clone().on_hover_text_at_pointer(tip);
        }
        let pressed = response.clicked() || response.dragged() || response.drag_started();
        if !pressed {
            self.last_seek = None;
            return;
        }
        let Some(t) = response.interact_pointer_pos().and_then(at) else { return };
        let Some(source) = node.source else { return };
        if node.bound {
            if response.clicked() || response.drag_started() {
                actions.push(Action::Message(
                    "the audio.file's position is bound to a channel, which decides where it is: unbind it to seek"
                        .to_string(),
                ));
            }
            return;
        }
        // A drag seeks as it moves, not every frame it stands.
        if self.last_seek.is_some_and(|last| (last - t).abs() < 0.05) && !response.clicked() {
            return;
        }
        self.last_seek = Some(t);
        actions.push(Action::SeekAudio { node: source, seconds: t });
    }
}
