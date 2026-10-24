//! The editor's view state, kept in `layout.ron` next to `project.ron`: pan and zoom, which
//! sections of the right column are open, the column's width, the viewer's aspect and the
//! editor's options. View state, not content: it does not touch the project file or the change
//! log.
//!
//! Where nodes sit is content: each node's position is in the network
//! ([`penumbra_graph::NodeDef::pos`]) and a move is an edit through the history like any other.
//! The layout keeps the positions of the nodes for drawing (by id, so a rename keeps its place),
//! with two additions of its own: where nodes are being dragged to (shown at once, recorded as
//! one edit when the drag ends, [`Layout::end_move`]), and where it placed nodes that came
//! without a position.
//!
//! A node without a position (new, or created by Claude or the project file without one) is
//! placed automatically: right of its first placed input, otherwise at the first free spot in
//! the part of the editor on screen (in a column by its depth when nothing is placed yet), and
//! moved down until it overlaps nothing. That place goes into the document outside the history
//! ([`Layout::take_placed`], `Session::place_unplaced`), so the project is written with it and
//! opens the same the next time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use penumbra_core::{NodeId, NodePath};
use penumbra_graph::{Network, Op, ThumbnailMode};
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "layout.ron";
/// Largest layout file read; anything bigger is not a layout.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Delay between the last change and writing the file.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// Space a node takes in the editor, in graph units, for placing and overlap checks.
pub const NODE_SIZE: Vec2 = vec2(150.0, 118.0);
/// The editor's zoom range: far enough out for a large network, far enough in to read a
/// thumbnail closely.
pub const ZOOM: (f32, f32) = (0.15, 6.0);
pub(crate) const GAP: Vec2 = vec2(50.0, 24.0);
/// The right column's narrowest width, in points.
pub const COLUMN_MIN: f32 = 260.0;
/// The right column's share of the window to start with.
const COLUMN_DEFAULT: f32 = 0.3;
/// The shares of the window the right column may take.
const COLUMN_SHARES: (f32, f32) = (0.05, 0.9);

/// The right column's width for its share of the window: never narrower than [`COLUMN_MIN`]
/// (unless the window itself is), never wider than the window.
pub fn column_width(share: f32, window: f32) -> f32 {
    (share * window).max(COLUMN_MIN).min(window).max(0.0)
}

/// The share of the window a column of `width` takes, for a column dragged to that width.
pub fn column_share(width: f32, window: f32) -> f32 {
    if !(width.is_finite() && window.is_finite()) || window <= 0.0 {
        return COLUMN_DEFAULT;
    }
    ((width / window).clamp(COLUMN_SHARES.0, COLUMN_SHARES.1) * 1000.0).round() / 1000.0
}

/// The viewer's picture area to start with and after a reset: 16:9, width over height.
pub const VIEWER_ASPECT: f32 = 16.0 / 9.0;
/// The aspects (width over height) a viewer dragged by hand may keep.
const VIEWER_ASPECTS: (f32, f32) = (0.25, 8.0);
/// The viewer's lowest height, header included, in points.
pub const VIEWER_MIN: f32 = 100.0;
/// Height the viewer leaves to the parameters above it, header included, in points.
pub const PARAMS_ROOM: f32 = 160.0;

/// The viewer's height for a picture area `width` wide with `aspect` (width over height), plus
/// `overhead` (header rows, frame). `room` is the height the viewer and the parameters share:
/// the viewer leaves [`PARAMS_ROOM`] of it and gets shorter rather than push the parameters
/// away, but never below [`VIEWER_MIN`].
pub fn viewer_height(aspect: f32, width: f32, overhead: f32, room: f32) -> f32 {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { VIEWER_ASPECT };
    let picture = picture_height(aspect, width);
    let height = if (picture + overhead).is_finite() { picture + overhead } else { VIEWER_MIN };
    height.min(viewer_max(room)).max(VIEWER_MIN)
}

/// The tallest the viewer may be when it shares `room` with the parameters.
pub fn viewer_max(room: f32) -> f32 {
    (room - PARAMS_ROOM).max(VIEWER_MIN)
}

/// The height of a picture area `width` wide whose picture (inset by
/// [`crate::viewer::INSET`] on every side) has `aspect`.
fn picture_height(aspect: f32, width: f32) -> f32 {
    let inset = 2.0 * crate::viewer::INSET;
    (width - inset).max(0.0) / aspect.max(f32::EPSILON) + inset
}

/// The aspect (width over height) of the picture inside a picture area of `size`, for a viewer
/// dragged to that size; `None` for nothing sensible.
pub fn viewer_aspect(size: Vec2) -> Option<f32> {
    let inset = 2.0 * crate::viewer::INSET;
    let (w, h) = (size.x - inset, size.y - inset);
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(((w / h).clamp(VIEWER_ASPECTS.0, VIEWER_ASPECTS.1) * 1000.0).round() / 1000.0)
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRepr {
    version: u32,
    #[serde(default)]
    view: Option<ViewRepr>,
    /// Node positions as older builds kept them here, by path. The project takes them over as
    /// it opens (`penumbra_project::legacy`); they are read only to know that the file is to be
    /// written again without them.
    #[serde(default, skip_serializing)]
    nodes: BTreeMap<String, [f32; 2]>,
    #[serde(default)]
    sections: Option<Sections>,
    #[serde(default)]
    options: Option<EditorOptions>,
    /// The right column's width as a share of the window's.
    #[serde(default)]
    column: Option<f32>,
    /// The viewer's picture aspect (width over height) when dragged by hand; none: 16:9.
    #[serde(default)]
    viewer_aspect: Option<f32>,
}

/// Which sections of the right column are open.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Sections {
    pub params: bool,
    pub viewer: bool,
    pub changes: bool,
}

impl Default for Sections {
    fn default() -> Self {
        Self { params: true, viewer: true, changes: true }
    }
}

/// What the editor's own menu (a right-click on empty space) switches, and the viewer's channel
/// scale, kept per project. The thumbnail mode and rate, also in that menu, are project settings
/// (`cook.thumbnails`), not view state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EditorOptions {
    /// The bar under a single channel's value (green, red while negative).
    pub chan_bars: bool,
    /// The viewer's picture behind the network, filling the editor.
    pub viewer_background: bool,
    /// Every binding as a line; off, a stub on the bound node, and a line only for the selected
    /// or hovered nodes.
    pub binding_lines: bool,
    /// How the viewer scales several channels.
    pub chan_scale: ChanScale,
    /// What the viewer shows of a `chan.audio_analysis`: its meters or the channel plot.
    pub audio_view: AudioView,
    /// The thumbnail mode and rate as older builds kept them here, read so their files load
    /// (the project takes them over as it opens, `penumbra_project::legacy`), never written.
    #[serde(default, skip_serializing, rename = "thumbnails", deserialize_with = "present")]
    pub(crate) old_thumbnails: Option<ThumbnailMode>,
    #[serde(default, skip_serializing, rename = "thumbnail_every", deserialize_with = "present")]
    pub(crate) old_thumbnail_every: Option<u32>,
}

/// A field that older files wrote as a plain value, read as `Some`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// What the viewer shows of an audio analysis.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioView {
    /// Level and band meters, beat and bar, tempo, hits and the spectrum, to read at a glance.
    #[default]
    Meters,
    /// The channel plot, as for any chan node.
    Plot,
}

impl AudioView {
    pub const ALL: [AudioView; 2] = [AudioView::Meters, AudioView::Plot];

    /// The name in `layout.ron` and in `ui.options`.
    pub fn name(self) -> &'static str {
        match self {
            AudioView::Meters => "meters",
            AudioView::Plot => "plot",
        }
    }

    pub fn parse(s: &str) -> Option<AudioView> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }

    /// The other one, for the header's toggle.
    pub fn next(self) -> AudioView {
        match self {
            AudioView::Meters => AudioView::Plot,
            AudioView::Plot => AudioView::Meters,
        }
    }
}

/// How the viewer scales the curves of several channels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChanScale {
    /// Per channel when one that moves would be flat on the shared scale, else shared.
    #[default]
    Auto,
    /// All curves on one scale, to compare them.
    Shared,
    /// A lane per channel, each with its own scale.
    PerChannel,
}

impl ChanScale {
    pub const ALL: [ChanScale; 3] = [ChanScale::Auto, ChanScale::Shared, ChanScale::PerChannel];

    /// The name in `layout.ron` and in `ui.options`.
    pub fn name(self) -> &'static str {
        match self {
            ChanScale::Auto => "auto",
            ChanScale::Shared => "shared",
            ChanScale::PerChannel => "per_channel",
        }
    }

    pub fn parse(s: &str) -> Option<ChanScale> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }

    /// The one after this, for a button that steps through them.
    pub fn next(self) -> ChanScale {
        match self {
            ChanScale::Auto => ChanScale::Shared,
            ChanScale::Shared => ChanScale::PerChannel,
            ChanScale::PerChannel => ChanScale::Auto,
        }
    }
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            chan_bars: true,
            viewer_background: false,
            binding_lines: true,
            chan_scale: ChanScale::Auto,
            audio_view: AudioView::Meters,
            old_thumbnails: None,
            old_thumbnail_every: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct ViewRepr {
    pan: [f32; 2],
    zoom: f32,
}

/// Pan and zoom of the editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Graph point shown at the top left of the canvas.
    pub origin: Vec2,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self { origin: vec2(-40.0, -40.0), zoom: 1.0 }
    }
}

pub struct Layout {
    file: Option<PathBuf>,
    /// Where every node of the network is drawn: its position in the network, unless it is
    /// being dragged or was placed here and the network does not have that place yet.
    pos: HashMap<NodeId, Pos2>,
    /// Each node's parent: positions are per component (each component's inside has a plane of
    /// its own), so overlap checks and bounds look at one level only.
    parents: HashMap<NodeId, Option<NodeId>>,
    /// The component whose inside the editor shows (`None`: the top level); `visible` and
    /// `bounds` refer to it. The one place that says which level is shown ([`Layout::enter`]).
    level: Option<NodeId>,
    /// Pan and zoom of the levels not shown, for coming back.
    level_views: HashMap<Option<NodeId>, View>,
    pub view: View,
    pub sections: Sections,
    pub options: EditorOptions,
    /// The right column's share of the window's width ([`column_width`]).
    pub column: f32,
    /// The viewer's picture aspect (width over height) after the user dragged its edge; `None`
    /// keeps it at [`VIEWER_ASPECT`]. Its height follows the column's width either way.
    pub viewer_aspect: Option<f32>,
    /// The part of the graph on screen, from the editor; new nodes without a placed input go
    /// there.
    pub visible: Option<Rect>,
    /// Where the nodes being dragged are now, ahead of the edit that records the drag.
    dragged: HashMap<NodeId, Pos2>,
    /// The nodes of the current drag.
    moving: Option<Vec<NodeId>>,
    /// Places given here to nodes that have none in the network, until the network has them.
    placed: HashMap<NodeId, Pos2>,
    /// Of those, the ones not yet handed to the document ([`Layout::take_placed`]).
    unsent: Vec<NodeId>,
    dirty_since: Option<Instant>,
    last_text: Option<String>,
}

impl Layout {
    /// The layout of the project in `dir`; `None` keeps it in memory only. A missing or broken
    /// file gives an empty layout (logged), never an error: positions are only convenience.
    pub fn load(dir: Option<&Path>) -> Self {
        let file = dir.map(|d| d.join(FILE_NAME));
        let mut layout = Self {
            file: file.clone(),
            pos: HashMap::new(),
            parents: HashMap::new(),
            level: None,
            level_views: HashMap::new(),
            view: View::default(),
            sections: Sections::default(),
            options: EditorOptions::default(),
            column: COLUMN_DEFAULT,
            viewer_aspect: None,
            visible: None,
            dragged: HashMap::new(),
            moving: None,
            placed: HashMap::new(),
            unsent: Vec::new(),
            dirty_since: None,
            last_text: None,
        };
        let Some(file) = file else { return layout };
        match read(&file) {
            Ok(Some((repr, text))) => {
                // Positions from an older build: the project has taken them over, so the file
                // is written again without them.
                if !repr.nodes.is_empty() {
                    layout.dirty_since = Some(Instant::now());
                }
                if let Some(v) = repr.view
                    && v.pan.iter().all(|c| c.is_finite())
                    && v.zoom.is_finite()
                {
                    layout.view =
                        View { origin: vec2(v.pan[0], v.pan[1]), zoom: v.zoom.clamp(ZOOM.0, ZOOM.1) };
                }
                if let Some(sections) = repr.sections {
                    layout.sections = sections;
                }
                if let Some(mut options) = repr.options {
                    // Project settings now: written again without them.
                    if options.old_thumbnails.is_some() || options.old_thumbnail_every.is_some() {
                        layout.dirty_since = Some(Instant::now());
                    }
                    (options.old_thumbnails, options.old_thumbnail_every) = (None, None);
                    layout.options = options;
                }
                if let Some(share) = repr.column.filter(|s| s.is_finite()) {
                    layout.column = share.clamp(COLUMN_SHARES.0, COLUMN_SHARES.1);
                }
                layout.viewer_aspect = repr
                    .viewer_aspect
                    .filter(|a| a.is_finite() && *a > 0.0)
                    .map(|a| a.clamp(VIEWER_ASPECTS.0, VIEWER_ASPECTS.1));
                layout.last_text = Some(text);
            }
            Ok(None) => {}
            Err(e) => log::warn!("{e}; starting with an empty layout"),
        }
        layout
    }

    /// Where node `id` is drawn.
    pub fn get(&self, id: NodeId) -> Option<Pos2> {
        self.pos.get(&id).copied()
    }

    pub fn set_view(&mut self, view: View, now: Instant) {
        if self.view != view {
            self.view = view;
            self.touch(now);
        }
    }

    pub fn set_options(&mut self, options: EditorOptions, now: Instant) {
        if self.options != options {
            self.options = options;
            self.touch(now);
        }
    }

    pub fn set_column(&mut self, share: f32, now: Instant) {
        if share.is_finite() && self.column != share {
            self.column = share.clamp(COLUMN_SHARES.0, COLUMN_SHARES.1);
            self.touch(now);
        }
    }

    /// The viewer's picture aspect kept after a drag of its edge; `None` goes back to 16:9.
    pub fn set_viewer_aspect(&mut self, aspect: Option<f32>, now: Instant) {
        let aspect =
            aspect.filter(|a| a.is_finite() && *a > 0.0).map(|a| a.clamp(VIEWER_ASPECTS.0, VIEWER_ASPECTS.1));
        if self.viewer_aspect != aspect {
            self.viewer_aspect = aspect;
            self.touch(now);
        }
    }

    pub fn set_sections(&mut self, sections: Sections, now: Instant) {
        if self.sections != sections {
            self.sections = sections;
            self.touch(now);
        }
    }

    /// The component whose inside the editor shows; `None` for the top level.
    pub fn level(&self) -> Option<NodeId> {
        self.level
    }

    /// Shows the inside of `level`; returns whether it had a view to come back to (otherwise the
    /// editor frames it).
    pub fn enter(&mut self, level: Option<NodeId>, now: Instant) -> bool {
        if level == self.level {
            return true;
        }
        self.level_views.insert(self.level, self.view);
        self.level = level;
        match self.level_views.get(&level) {
            Some(v) => {
                let v = *v;
                self.set_view(v, now);
                true
            }
            None => false,
        }
    }

    fn touch(&mut self, now: Instant) {
        self.dirty_since.get_or_insert(now);
    }

    /// A drag of `ids` starts; [`Layout::end_move`] turns it into one edit.
    pub fn begin_move(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.dragged.clear();
        self.moving = Some(ids.into_iter().filter(|id| self.pos.contains_key(id)).collect());
    }

    /// Moves the nodes being dragged by `delta`: shown at once, recorded when the drag ends.
    pub fn drag_by(&mut self, delta: Vec2) {
        let Some(ids) = &self.moving else { return };
        if !(delta.x.is_finite() && delta.y.is_finite()) {
            return;
        }
        for id in ids {
            if let Some(p) = self.pos.get_mut(id) {
                *p += delta;
                self.dragged.insert(*id, *p);
            }
        }
    }

    /// The drag ended: the edit that puts the dragged nodes where they went, as one history
    /// entry; none when nothing moved. Until it is applied the nodes are drawn where the
    /// network has them (an edit refused leaves them there).
    pub fn end_move(&mut self, net: &Network) -> Option<Vec<Op>> {
        self.moving.take()?;
        let to = std::mem::take(&mut self.dragged);
        let ops = place_ops(net, &to);
        (!ops.is_empty()).then_some(ops)
    }

    /// The places given to nodes without a position since the last call, for the document
    /// (`Session::place_unplaced`): written outside the history, so the project opens the same
    /// the next time.
    pub fn take_placed(&mut self) -> Vec<(NodeId, [f32; 2])> {
        let placed = &self.placed;
        self.unsent.drain(..).filter_map(|id| placed.get(&id).map(|p| (id, [p.x, p.y]))).collect()
    }

    /// Takes the positions from the network and places nodes without one. Cheap enough to run
    /// every frame.
    pub fn sync(&mut self, net: &Network) {
        if self.level.is_some_and(|l| net.node(l).is_none()) {
            self.level = None;
        }
        self.placed.retain(|id, _| net.node(*id).is_some_and(|n| n.pos.is_none()));
        self.dragged.retain(|id, _| net.node(*id).is_some());
        self.pos.clear();
        self.parents.clear();
        let mut fresh: Vec<NodeId> = Vec::new();
        for node in net.nodes() {
            self.parents.insert(node.id, node.parent);
            let at = self
                .dragged
                .get(&node.id)
                .copied()
                .or(node.pos.map(|[x, y]| pos2(x, y)))
                .or_else(|| self.placed.get(&node.id).copied());
            match at {
                Some(p) => {
                    self.pos.insert(node.id, p);
                }
                None => fresh.push(node.id),
            }
        }
        if fresh.is_empty() {
            return;
        }
        // Upstream first, so a chain of new nodes is placed left to right.
        let depth = depths(net);
        fresh.sort_by_key(|id| (depth.get(id).copied().unwrap_or(0), *id));
        let mut column_rows: BTreeMap<(Option<NodeId>, usize), f32> = BTreeMap::new();
        // Per level, as it was before this round: whether anything was placed, and where a new
        // block of columns starts.
        let mut levels: HashMap<Option<NodeId>, (bool, Pos2)> = HashMap::new();
        for id in &fresh {
            let parent = self.parent_of(*id);
            levels.entry(parent).or_insert_with(|| {
                let any = self.pos.keys().any(|k| self.parent_of(*k) == parent);
                let base =
                    self.bounds_of(parent).map_or(pos2(0.0, 0.0), |b| pos2(b.min.x, b.max.y + GAP.y * 2.0));
                (any, base)
            });
        }
        for id in fresh {
            let parent = self.parent_of(id);
            let (any_placed, base) = levels.get(&parent).copied().unwrap_or((false, pos2(0.0, 0.0)));
            // Beside an input on the same level (an input node's source lies outside).
            let beside = net.node(id).and_then(|n| {
                n.inputs.iter().flatten().find_map(|port| {
                    (self.parent_of(port.node) == parent).then(|| self.pos.get(&port.node).copied()).flatten()
                })
            });
            let shown = parent == self.level;
            let on_screen =
                if any_placed && shown { self.visible.and_then(|v| self.free_in(v, parent)) } else { None };
            let wanted = match (beside, on_screen) {
                (Some(p), _) => p + vec2(NODE_SIZE.x + GAP.x, 0.0),
                (None, Some(p)) => p,
                (None, None) => {
                    let d = depth.get(&id).copied().unwrap_or(0);
                    let row = column_rows.entry((parent, d)).or_insert(0.0);
                    let p = base + vec2(d as f32 * (NODE_SIZE.x + GAP.x), *row);
                    *row += NODE_SIZE.y + GAP.y;
                    p
                }
            };
            // Nodes placed beside their input do not count in `column_rows`, so check every spot.
            // Whole units, as the network keeps positions.
            let p = self.free_spot(wanted, parent).round();
            self.pos.insert(id, p);
            self.placed.insert(id, p);
            self.unsent.push(id);
        }
    }

    fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.parents.get(&id).copied().flatten()
    }

    /// The first free place in `area` (graph units), by columns from the left, top to bottom,
    /// keeping a margin from its edges.
    fn free_in(&self, area: Rect, level: Option<NodeId>) -> Option<Pos2> {
        let area = area.shrink2(GAP);
        let step = NODE_SIZE + GAP;
        let cols = ((area.width() - NODE_SIZE.x) / step.x).floor();
        let rows = ((area.height() - NODE_SIZE.y) / step.y).floor();
        if !(cols >= 0.0 && rows >= 0.0) {
            return None;
        }
        let (cols, rows) = ((cols as usize).min(100), (rows as usize).min(100));
        (0..=cols)
            .flat_map(|c| (0..=rows).map(move |r| area.min + vec2(c as f32 * step.x, r as f32 * step.y)))
            .find(|p| !self.overlaps(*p, level))
    }

    fn overlaps(&self, p: Pos2, level: Option<NodeId>) -> bool {
        let r = Rect::from_min_size(p, NODE_SIZE);
        self.pos
            .iter()
            .filter(|(id, _)| self.parent_of(**id) == level)
            .any(|(_, q)| Rect::from_min_size(*q, NODE_SIZE).expand2(GAP * 0.4).intersects(r))
    }

    /// The first place at or below `p` where a node overlaps nothing.
    fn free_spot(&self, mut p: Pos2, level: Option<NodeId>) -> Pos2 {
        for _ in 0..1000 {
            if !self.overlaps(p, level) {
                break;
            }
            p.y += NODE_SIZE.y + GAP.y;
        }
        p
    }

    /// Bounding box of the placed nodes of the level shown, in graph units.
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds_of(self.level)
    }

    /// Bounding box of the placed nodes inside `level`.
    pub fn bounds_of(&self, level: Option<NodeId>) -> Option<Rect> {
        self.pos
            .iter()
            .filter(|(id, _)| self.parent_of(**id) == level)
            .map(|(_, p)| Rect::from_min_size(*p, NODE_SIZE))
            .reduce(|a, b| a.union(b))
    }

    /// Writes the file about a second after the last change; `force` writes now (closing).
    pub fn save_if_due(&mut self, now: Instant, force: bool) {
        let Some(since) = self.dirty_since else { return };
        if !force && now.saturating_duration_since(since) < SAVE_DELAY {
            return;
        }
        self.dirty_since = None;
        let Some(file) = self.file.clone() else { return };
        // The file keeps the top level's view.
        let top = match self.level {
            None => self.view,
            Some(_) => self.level_views.get(&None).copied().unwrap_or(self.view),
        };
        let repr = FileRepr {
            version: 1,
            view: Some(ViewRepr { pan: [top.origin.x, top.origin.y], zoom: top.zoom }),
            nodes: BTreeMap::new(),
            sections: Some(self.sections),
            options: Some(self.options),
            column: Some(self.column),
            viewer_aspect: self.viewer_aspect,
        };
        let text = match ron::ser::to_string_pretty(&repr, ron::ser::PrettyConfig::default()) {
            Ok(t) => format!(
                "// penumbra editor layout: pan and zoom, open sections, editor options. Not part of the project content.\n{t}\n"
            ),
            Err(e) => {
                log::warn!("cannot write {}: {e}", file.display());
                return;
            }
        };
        if self.last_text.as_deref() == Some(text.as_str()) {
            return;
        }
        if let Err(e) = write_atomic(&file, &text) {
            log::warn!("{e}");
            return;
        }
        self.last_text = Some(text);
    }

    /// Node paths with the places they are drawn at, for `ui.state`.
    pub fn positions(&self, net: &Network) -> BTreeMap<NodePath, [f32; 2]> {
        self.pos.iter().filter_map(|(id, p)| Some((net.path_of(*id)?, [p.x, p.y]))).collect()
    }
}

/// The edit that moves nodes to the places in `to`: one [`Op::Place`] per node whose position
/// in `net` differs (in whole units, as the network keeps them), in path order. Places that are
/// no place are left out.
pub fn place_ops(net: &Network, to: &HashMap<NodeId, Pos2>) -> Vec<Op> {
    let mut ops: Vec<(NodePath, Op)> = to
        .iter()
        .filter_map(|(id, p)| {
            let pos = penumbra_graph::check_pos([p.x, p.y]).ok()?;
            if net.node(*id)?.pos == Some(pos) {
                return None;
            }
            let path = net.path_of(*id)?;
            Some((path.clone(), Op::Place { path, pos: Some(pos) }))
        })
        .collect();
    ops.sort_by(|a, b| a.0.cmp(&b.0));
    ops.into_iter().map(|(_, op)| op).collect()
}

/// The history label of an edit of `ops` that moves nodes.
pub fn move_label(ops: &[Op]) -> String {
    match ops {
        [one] => one.label(),
        many => format!("move {} nodes", many.len()),
    }
}

/// Longest path from a node without inputs or bindings, per node, counted within its level.
///
/// A wire between levels (into a component's port node, or out of one) counts as a wire between
/// the two nodes on the level both chains of parents share: the component and its neighbour.
/// Counting the inner nodes' depth would push a component's downstream neighbours far right.
fn depths(net: &Network) -> HashMap<NodeId, usize> {
    let mut edges: Vec<(NodeId, NodeId)> = Vec::new();
    for node in net.nodes() {
        // Binding sources count like inputs, so a channel lands left of what it drives.
        for up in node.upstream() {
            if let Some(edge) = sibling_edge(net, up, node.id) {
                edges.push(edge);
            }
        }
    }
    let mut depth: HashMap<NodeId, usize> = net.nodes().map(|n| (n.id, 0)).collect();
    // Relax until stable. Within one level wires between components can form a loop, so the
    // rounds are bounded rather than run to a fixed point.
    for _ in 0..=net.len() {
        let mut changed = false;
        for (from, to) in &edges {
            let d = depth.get(from).copied().unwrap_or(0) + 1;
            if depth.get(to).is_none_or(|old| *old < d) {
                depth.insert(*to, d);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // A source sits just left of the first node it feeds, not in the far left column: a channel
    // that drives a transform deep in a chain lands beside it.
    let has_input: HashSet<NodeId> = edges.iter().map(|(_, to)| *to).collect();
    let mut pulled: HashMap<NodeId, usize> = HashMap::new();
    for (from, to) in &edges {
        if has_input.contains(from) {
            continue;
        }
        let d = depth.get(to).copied().unwrap_or(0).saturating_sub(1);
        pulled.entry(*from).and_modify(|old| *old = (*old).min(d)).or_insert(d);
    }
    depth.extend(pulled);
    depth
}

/// The wire `from` -> `to` as one between their ancestors (or themselves) on a shared level;
/// none when one holds the other.
fn sibling_edge(net: &Network, from: NodeId, to: NodeId) -> Option<(NodeId, NodeId)> {
    let chain = |id: NodeId| {
        let mut out = vec![id];
        let mut cur = id;
        while let Some(p) = net.node(cur).and_then(|n| n.parent) {
            if out.len() > net.len() {
                break;
            }
            out.push(p);
            cur = p;
        }
        out
    };
    let parent = |id: NodeId| net.node(id).and_then(|n| n.parent);
    let froms = chain(from);
    for a in chain(to) {
        if let Some(b) = froms.iter().find(|b| parent(**b) == parent(a)) {
            return (*b != a).then_some((*b, a));
        }
    }
    None
}

fn read(file: &Path) -> Result<Option<(FileRepr, String)>, String> {
    let meta = match std::fs::metadata(file) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    if meta.len() > MAX_BYTES {
        return Err(format!("{} is larger than {} MiB", file.display(), MAX_BYTES >> 20));
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let repr: FileRepr = ron::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    if repr.version != 1 {
        return Err(format!("{}: layout version {} is not supported", file.display(), repr.version));
    }
    Ok(Some((repr, text)))
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("ron.tmp");
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_graph::Op;

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: path.parse().unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn chain() -> Network {
        let types = penumbra_ops::builtin_types().unwrap();
        let ops = vec![
            create("/a", "chan.constant"),
            create("/b", "chan.math"),
            create("/c", "chan.math"),
            Op::Connect { from: "/a".parse().unwrap(), output: 0, to: "/b".parse().unwrap(), input: 0 },
            Op::Connect { from: "/b".parse().unwrap(), output: 0, to: "/c".parse().unwrap(), input: 0 },
        ];
        Network::new().apply(&types, &ops).unwrap().network
    }

    #[test]
    fn new_nodes_are_placed_by_depth_without_overlap() {
        let net = chain();
        let mut layout = Layout::load(None);
        layout.sync(&net);
        let p = |path: &str| layout.get(net.resolve(&path.parse().unwrap()).unwrap()).unwrap();
        assert!(p("/a").x < p("/b").x && p("/b").x < p("/c").x);
        assert_eq!(p("/a").y, p("/b").y);
        let rects: Vec<Rect> =
            ["/a", "/b", "/c"].iter().map(|n| Rect::from_min_size(p(n), NODE_SIZE)).collect();
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                assert!(!a.intersects(*b));
            }
        }
    }

    /// `net` with the places the layout gave its nodes, as the app hands them to the document.
    fn with_placed(net: &Network, placed: &[(NodeId, [f32; 2])]) -> Network {
        let types = penumbra_ops::builtin_types().unwrap();
        let ops: Vec<Op> = placed
            .iter()
            .filter_map(|(id, pos)| Some(Op::Place { path: net.path_of(*id)?, pos: Some(*pos) }))
            .collect();
        net.apply(&types, &ops).unwrap().network
    }

    #[test]
    fn a_drag_is_shown_at_once_and_recorded_as_one_edit() {
        let types = penumbra_ops::builtin_types().unwrap();
        let net = chain();
        let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
        let (a, b) = (id("/a"), id("/b"));
        let mut layout = Layout::load(None);
        layout.sync(&net);
        let placed = layout.take_placed();
        assert_eq!(placed.len(), 3, "each new node once");
        assert!(layout.take_placed().is_empty());
        let net = with_placed(&net, &placed);
        layout.sync(&net);
        let (a_was, b_was) = (layout.get(a).unwrap(), layout.get(b).unwrap());
        layout.begin_move([a, b]);
        for _ in 0..10 {
            layout.drag_by(vec2(10.25, -3.0));
            layout.sync(&net);
        }
        assert_eq!(layout.get(a), Some(a_was + vec2(102.5, -30.0)), "shown while dragging");
        let ops = layout.end_move(&net).unwrap();
        assert_eq!(move_label(&ops), "move 2 nodes");
        let moved = net.apply(&types, &ops).unwrap().network;
        layout.sync(&moved);
        // Whole units, as the network keeps them.
        assert_eq!(layout.get(a), Some((a_was + vec2(102.5, -30.0)).round()));
        assert_eq!(layout.get(b), Some((b_was + vec2(102.5, -30.0)).round()));
        // A drag that ends where it started, or a refused edit, leaves the network's places.
        layout.begin_move([a]);
        assert_eq!(layout.end_move(&moved), None);
        layout.begin_move([a]);
        layout.drag_by(vec2(500.0, 0.0));
        assert!(layout.end_move(&moved).is_some());
        layout.sync(&moved);
        assert_eq!(layout.get(a), Some((a_was + vec2(102.5, -30.0)).round()));
    }

    #[test]
    fn a_placed_node_stays_where_it_was_placed_until_the_network_has_it() {
        let net = chain();
        let a = net.resolve(&"/a".parse().unwrap()).unwrap();
        let mut layout = Layout::load(None);
        layout.sync(&net);
        let first = layout.get(a).unwrap();
        // The view moves away; the node does not follow it.
        layout.visible = Some(Rect::from_min_size(pos2(5000.0, 5000.0), vec2(800.0, 600.0)));
        layout.sync(&net);
        assert_eq!(layout.get(a), Some(first));
        // Once the network has a place, that one counts.
        let types = penumbra_ops::builtin_types().unwrap();
        let ops = [Op::Place { path: "/a".parse().unwrap(), pos: Some([7.0, 8.0]) }];
        let net = net.apply(&types, &ops).unwrap().network;
        layout.sync(&net);
        assert_eq!(layout.get(a), Some(pos2(7.0, 8.0)));
    }

    #[test]
    fn depth_counts_within_a_level() {
        let types = penumbra_ops::builtin_types().unwrap();
        let wire = |from: &str, to: &str| Op::Connect {
            from: from.parse().unwrap(),
            output: 0,
            to: to.parse().unwrap(),
            input: 0,
        };
        let ops = vec![
            create("/src", "chan.constant"),
            create("/comp", "comp.base"),
            create("/comp/in", "chan.in"),
            create("/comp/a", "chan.math"),
            create("/comp/b", "chan.math"),
            create("/comp/out", "chan.out"),
            create("/after", "chan.math"),
            wire("/src", "/comp/in"),
            wire("/comp/in", "/comp/a"),
            wire("/comp/a", "/comp/b"),
            wire("/comp/b", "/comp/out"),
            wire("/comp/out", "/after"),
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        let depth = depths(&net);
        let d = |path: &str| depth[&net.resolve(&path.parse().unwrap()).unwrap()];
        // Outside: the component sits one step after its source, its consumer one after it,
        // however deep the inside is.
        assert_eq!((d("/src"), d("/comp"), d("/after")), (0, 1, 2));
        assert_eq!((d("/comp/in"), d("/comp/a"), d("/comp/out")), (0, 1, 3));
    }

    #[test]
    fn a_new_node_without_inputs_lands_on_screen() {
        let net = chain();
        let mut layout = Layout::load(None);
        layout.sync(&net);
        let shown = Rect::from_min_size(pos2(2000.0, 2000.0), vec2(800.0, 600.0));
        layout.visible = Some(shown);
        let ops = [create("/d", "chan.constant")];
        let more = net.apply(&penumbra_ops::builtin_types().unwrap(), &ops).unwrap().network;
        layout.sync(&more);
        let d = layout.get(more.resolve(&"/d".parse().unwrap()).unwrap()).unwrap();
        assert!(shown.contains_rect(Rect::from_min_size(d, NODE_SIZE)), "{d:?}");
    }

    #[test]
    fn each_component_has_a_plane_of_its_own() {
        let types = penumbra_ops::builtin_types().unwrap();
        let ops = [create("/g", "comp.base"), create("/g/x", "chan.constant")];
        let net = chain().apply(&types, &ops).unwrap().network;
        let mut layout = Layout::load(None);
        layout.sync(&net);
        let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
        // Inside, nothing else is in the way: the first node takes the first place, which a
        // node outside holds too.
        assert_eq!(layout.get(id("/g/x")), layout.get(id("/a")));
        let outside = layout.bounds().unwrap();
        layout.enter(Some(id("/g")), Instant::now());
        assert_eq!(layout.bounds(), Some(Rect::from_min_size(layout.get(id("/g/x")).unwrap(), NODE_SIZE)));
        assert!(outside.width() > NODE_SIZE.x);
    }

    #[test]
    fn the_column_is_a_share_of_the_window() {
        assert_eq!(column_width(0.3, 2000.0), 600.0);
        assert_eq!(column_width(0.3, 800.0), COLUMN_MIN, "never narrower than its minimum");
        assert_eq!(column_width(0.3, 200.0), 200.0, "never wider than the window");
        assert_eq!(column_share(500.0, 2000.0), 0.25);
        assert_eq!(column_share(1990.0, 2000.0), COLUMN_SHARES.1);
        assert_eq!(column_share(f32::NAN, 2000.0), COLUMN_DEFAULT);
        assert_eq!(column_share(300.0, 0.0), COLUMN_DEFAULT);
        // A share dragged at one window size gives the same share at another.
        let share = column_share(column_width(0.35, 1600.0), 1600.0);
        assert!((column_width(share, 3200.0) - 1120.0).abs() < 1.0);
    }

    #[test]
    fn the_viewer_is_as_tall_as_its_picture_needs() {
        // A 16:9 picture fills the picture area inside its inset.
        let h = viewer_height(VIEWER_ASPECT, 488.0, 40.0, 2000.0);
        assert!((h - (480.0 * 9.0 / 16.0 + 8.0 + 40.0)).abs() < 0.01, "{h}");
        let back = viewer_aspect(vec2(488.0, h - 40.0)).unwrap();
        assert!((back - VIEWER_ASPECT).abs() < 0.001, "{back}");
        // Too little room: the parameters keep theirs, the viewer is shorter.
        assert_eq!(viewer_height(VIEWER_ASPECT, 1000.0, 40.0, 500.0), 500.0 - PARAMS_ROOM);
        // Never below its minimum, whatever the room.
        assert_eq!(viewer_height(VIEWER_ASPECT, 1000.0, 40.0, 50.0), VIEWER_MIN);
        assert_eq!(viewer_height(VIEWER_ASPECT, 0.0, 0.0, 2000.0), VIEWER_MIN);
        assert_eq!(viewer_height(f32::NAN, 488.0, 40.0, 2000.0), h, "16:9 for nonsense");
        assert_eq!(viewer_aspect(vec2(4.0, 300.0)), None);
        assert_eq!(viewer_aspect(vec2(10_000.0, 20.0)), Some(VIEWER_ASPECTS.1));
    }

    #[test]
    fn view_state_survives_a_round_trip_through_the_file() {
        let dir = penumbra_core::scratch::ScratchDir::new("layout").unwrap();
        let now = Instant::now();
        let mut layout = Layout::load(Some(&dir));
        let options = EditorOptions {
            chan_scale: ChanScale::PerChannel,
            audio_view: AudioView::Plot,
            ..EditorOptions::default()
        };
        layout.set_options(options, now);
        layout.set_column(0.42, now);
        layout.set_viewer_aspect(Some(2.5), now);
        layout.save_if_due(now, false);
        assert!(!dir.join(FILE_NAME).exists(), "not before the delay");
        layout.save_if_due(now + SAVE_DELAY, false);
        let again = Layout::load(Some(&dir));
        assert_eq!(again.options, options);
        assert_eq!(again.column, 0.42);
        assert_eq!(again.viewer_aspect, Some(2.5));
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        // One from before the thumbnails were a project setting loads, and is written again
        // without them.
        assert!(!text.contains("thumbnail"), "{text}");
        let old = text.replace(
            "chan_scale:",
            "thumbnails: when_free,\n        thumbnail_every: 3,\n        chan_scale:",
        );
        std::fs::write(dir.join(FILE_NAME), &old).unwrap();
        let mut loaded = Layout::load(Some(&dir));
        assert_eq!(loaded.options, options);
        loaded.save_if_due(Instant::now() + SAVE_DELAY, false);
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), text);
        assert!(text.contains("chan_scale: per_channel"), "{text}");
        // A file from before the channel scale existed still loads, with the default.
        std::fs::write(dir.join(FILE_NAME), text.replace("chan_scale: per_channel,", "")).unwrap();
        assert_eq!(Layout::load(Some(&dir)).options.chan_scale, ChanScale::Auto);
        assert!(text.contains("audio_view: plot"), "{text}");
        std::fs::write(dir.join(FILE_NAME), text.replace("audio_view: plot,", "")).unwrap();
        assert_eq!(Layout::load(Some(&dir)).options.audio_view, AudioView::Meters);
        // One from before the viewer kept a dragged height loads with the automatic 16:9.
        assert!(text.contains("viewer_aspect: Some(2.5)"), "{text}");
        std::fs::write(dir.join(FILE_NAME), text.replace("viewer_aspect: Some(2.5),", "")).unwrap();
        assert_eq!(Layout::load(Some(&dir)).viewer_aspect, None);
        // One from before positions moved into the project loads, and is written again
        // without them.
        assert!(!text.contains("nodes"), "{text}");
        let old = text.replace("viewer_aspect:", "nodes: {\"/a\": (10.0, 20.0)},\n    viewer_aspect:");
        std::fs::write(dir.join(FILE_NAME), &old).unwrap();
        let mut loaded = Layout::load(Some(&dir));
        assert_eq!(loaded.options, options);
        loaded.save_if_due(Instant::now() + SAVE_DELAY, false);
        let rewritten = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(!rewritten.contains("nodes"), "{rewritten}");
        std::fs::write(dir.join(FILE_NAME), "garbage").unwrap();
        let broken = Layout::load(Some(&dir));
        assert_eq!(broken.options, EditorOptions::default());
    }
}
