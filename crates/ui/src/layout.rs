//! Where nodes sit in the editor, kept in `layout.ron` next to `project.ron`, with pan, zoom and
//! which sections of the right column are open. View state, not content: it does not touch the
//! project file or the change log. Moves are undoable all the same, on a stack of their own that
//! Ctrl+Z interleaves with the history by time ([`Layout::undo_move`]).
//!
//! Positions are held by node id while the app runs, so a rename keeps its place; the file keys
//! them by path. A node without a position (new, or created by Claude or the project file) is
//! placed automatically: right of its first placed input, otherwise at the first free spot in
//! the part of the editor on screen (in a column by its depth when nothing is placed yet), and
//! moved down until it overlaps nothing. A deleted node's position is remembered for the run,
//! so undoing the delete brings it back where it was.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use penumbra_core::{NodeId, NodePath};
use penumbra_graph::Network;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "layout.ron";
/// Largest layout file read; anything bigger is not a layout.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Delay between the last move and writing the file.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// Space a node takes in the editor, in graph units, for placing and overlap checks.
pub const NODE_SIZE: Vec2 = vec2(150.0, 118.0);
const GAP: Vec2 = vec2(50.0, 24.0);

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRepr {
    version: u32,
    #[serde(default)]
    view: Option<ViewRepr>,
    #[serde(default)]
    nodes: BTreeMap<String, [f32; 2]>,
    #[serde(default)]
    sections: Option<Sections>,
    #[serde(default)]
    options: Option<EditorOptions>,
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

/// What the editor's own menu (a right-click on empty space) switches; thumbnails are a
/// command of the engine's (`cook.thumbnails`) and not kept here.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EditorOptions {
    /// The bar under a single channel's value (green, red while negative).
    pub chan_bars: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self { chan_bars: true }
    }
}

/// One drag of one or more nodes: where each was and where it went.
struct Move {
    at: SystemTime,
    nodes: Vec<(NodeId, Pos2, Pos2)>,
}

/// Moves kept for undo.
const MOVES_KEPT: usize = 200;

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
    pos: HashMap<NodeId, Pos2>,
    /// Each placed node's parent: positions are per component (each component's inside has a
    /// plane of its own), so overlap checks and bounds look at one level only.
    parents: HashMap<NodeId, Option<NodeId>>,
    /// The component whose inside the editor shows (`None`: the top level); `visible` and
    /// `bounds` refer to it.
    pub level: Option<NodeId>,
    /// Pan and zoom of the levels not shown, for coming back.
    level_views: HashMap<Option<NodeId>, View>,
    /// Positions from the file for nodes not seen yet, by path.
    loaded: BTreeMap<String, Pos2>,
    pub view: View,
    pub sections: Sections,
    pub options: EditorOptions,
    /// The part of the graph on screen, from the editor; new nodes without a placed input go
    /// there.
    pub visible: Option<Rect>,
    /// Positions of nodes deleted during this run, for an undo that brings them back.
    gone: HashMap<NodeId, Pos2>,
    /// Positions when the current drag started.
    moving: Option<Vec<(NodeId, Pos2)>>,
    moves: Vec<Move>,
    undone: Vec<Move>,
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
            loaded: BTreeMap::new(),
            view: View::default(),
            sections: Sections::default(),
            options: EditorOptions::default(),
            visible: None,
            gone: HashMap::new(),
            moving: None,
            moves: Vec::new(),
            undone: Vec::new(),
            dirty_since: None,
            last_text: None,
        };
        let Some(file) = file else { return layout };
        match read(&file) {
            Ok(Some((repr, text))) => {
                layout.loaded = repr
                    .nodes
                    .into_iter()
                    .filter(|(_, p)| p.iter().all(|c| c.is_finite()))
                    .map(|(k, [x, y])| (k, pos2(x, y)))
                    .collect();
                if let Some(v) = repr.view
                    && v.pan.iter().all(|c| c.is_finite())
                    && v.zoom.is_finite()
                {
                    layout.view = View { origin: vec2(v.pan[0], v.pan[1]), zoom: v.zoom.clamp(0.1, 4.0) };
                }
                if let Some(sections) = repr.sections {
                    layout.sections = sections;
                }
                if let Some(options) = repr.options {
                    layout.options = options;
                }
                layout.last_text = Some(text);
            }
            Ok(None) => {}
            Err(e) => log::warn!("{e}; starting with an empty layout"),
        }
        layout
    }

    pub fn get(&self, id: NodeId) -> Option<Pos2> {
        self.pos.get(&id).copied()
    }

    pub fn set(&mut self, id: NodeId, pos: Pos2, now: Instant) {
        if !(pos.x.is_finite() && pos.y.is_finite()) {
            return;
        }
        if self.pos.insert(id, pos) != Some(pos) {
            self.touch(now);
        }
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

    pub fn set_sections(&mut self, sections: Sections, now: Instant) {
        if self.sections != sections {
            self.sections = sections;
            self.touch(now);
        }
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

    /// A drag of `ids` starts; [`Layout::end_move`] records it as one undo step.
    pub fn begin_move(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        let start = ids.into_iter().filter_map(|id| Some((id, self.get(id)?))).collect();
        self.moving = Some(start);
    }

    /// The drag ended; records it unless nothing moved.
    pub fn end_move(&mut self) {
        let Some(start) = self.moving.take() else { return };
        let nodes: Vec<(NodeId, Pos2, Pos2)> = start
            .into_iter()
            .filter_map(|(id, from)| Some((id, from, self.get(id)?)))
            .filter(|(_, from, to)| from != to)
            .collect();
        if nodes.is_empty() {
            return;
        }
        self.moves.push(Move { at: SystemTime::now(), nodes });
        if self.moves.len() > MOVES_KEPT {
            self.moves.remove(0);
        }
        self.undone.clear();
    }

    /// When the newest undoable move was made.
    pub fn last_move(&self) -> Option<SystemTime> {
        self.moves.last().map(|m| m.at)
    }

    /// When the move a redo would bring back was made.
    pub fn next_redo(&self) -> Option<SystemTime> {
        self.undone.last().map(|m| m.at)
    }

    /// Puts the nodes of the newest move back; whether there was one.
    pub fn undo_move(&mut self, now: Instant) -> bool {
        let Some(m) = self.moves.pop() else { return false };
        for (id, from, _) in &m.nodes {
            self.place_again(*id, *from, now);
        }
        self.undone.push(m);
        true
    }

    pub fn redo_move(&mut self, now: Instant) -> bool {
        let Some(m) = self.undone.pop() else { return false };
        for (id, _, to) in &m.nodes {
            self.place_again(*id, *to, now);
        }
        self.moves.push(m);
        true
    }

    /// Sets a position for a node that may be deleted for now.
    fn place_again(&mut self, id: NodeId, p: Pos2, now: Instant) {
        if self.pos.contains_key(&id) {
            self.set(id, p, now);
        } else if self.gone.contains_key(&id) {
            self.gone.insert(id, p);
        }
    }

    /// Forgets deleted nodes and places new ones. Cheap when nothing changed.
    pub fn sync(&mut self, net: &Network, now: Instant) {
        let gone = &mut self.gone;
        let parents = &mut self.parents;
        self.pos.retain(|id, p| {
            let keep = net.node(*id).is_some();
            if !keep {
                gone.insert(*id, *p);
                parents.remove(id);
            }
            keep
        });
        if self.level.is_some_and(|l| net.node(l).is_none()) {
            self.level = None;
        }
        if self.pos.len() == net.len() && self.parents.len() == net.len() {
            return;
        }
        let mut fresh: Vec<NodeId> = Vec::new();
        for node in net.nodes() {
            self.parents.insert(node.id, node.parent);
            if self.pos.contains_key(&node.id) {
                continue;
            }
            let back = self.gone.remove(&node.id);
            let from_file =
                back.or_else(|| net.path_of(node.id).and_then(|p| self.loaded.remove(&p.to_string())));
            match from_file {
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
            let p = self.free_spot(wanted, parent);
            self.pos.insert(id, p);
        }
        self.touch(now);
    }

    /// Lays out every node again by depth, as for a new project.
    pub fn arrange(&mut self, net: &Network, now: Instant) {
        self.pos.clear();
        self.loaded.clear();
        self.sync(net, now);
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
    pub fn save_if_due(&mut self, net: &Network, now: Instant, force: bool) {
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
        let mut repr = FileRepr {
            version: 1,
            view: Some(ViewRepr { pan: [top.origin.x, top.origin.y], zoom: top.zoom }),
            nodes: BTreeMap::new(),
            sections: Some(self.sections),
            options: Some(self.options),
        };
        for (id, p) in &self.pos {
            if let Some(path) = net.path_of(*id) {
                repr.nodes.insert(path.to_string(), [round(p.x), round(p.y)]);
            }
        }
        // Positions of nodes that are gone for now (a file edit in progress) are kept.
        for (path, p) in &self.loaded {
            repr.nodes.entry(path.clone()).or_insert([round(p.x), round(p.y)]);
        }
        let text = match ron::ser::to_string_pretty(&repr, ron::ser::PrettyConfig::default()) {
            Ok(t) => format!(
                "// penumbra editor layout: node positions, pan and zoom. Not part of the project content.\n{t}\n"
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

    /// Node paths with positions, for `ui.state`.
    pub fn positions(&self, net: &Network) -> BTreeMap<NodePath, [f32; 2]> {
        self.pos.iter().filter_map(|(id, p)| Some((net.path_of(*id)?, [p.x, p.y]))).collect()
    }
}

fn round(v: f32) -> f32 {
    v.round()
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
        layout.sync(&net, Instant::now());
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

    #[test]
    fn moves_undo_and_redo_and_a_deleted_node_comes_back_in_place() {
        let net = chain();
        let a = net.resolve(&"/a".parse().unwrap()).unwrap();
        let now = Instant::now();
        let mut layout = Layout::load(None);
        layout.sync(&net, now);
        let start = layout.get(a).unwrap();
        layout.begin_move([a]);
        layout.set(a, pos2(900.0, 900.0), now);
        layout.end_move();
        assert!(layout.last_move().is_some());
        assert!(layout.undo_move(now));
        assert_eq!(layout.get(a), Some(start));
        assert!(layout.redo_move(now));
        assert_eq!(layout.get(a), Some(pos2(900.0, 900.0)));
        // A drag that ends where it started records nothing.
        layout.begin_move([a]);
        layout.end_move();
        assert!(layout.undo_move(now) && !layout.undo_move(now));

        let ops = [Op::Delete { path: "/a".parse().unwrap() }];
        let without = net.apply(&penumbra_ops::builtin_types().unwrap(), &ops).unwrap().network;
        layout.sync(&without, now);
        assert_eq!(layout.get(a), None);
        layout.sync(&net, now);
        assert_eq!(layout.get(a), Some(start));
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
        layout.sync(&net, Instant::now());
        let shown = Rect::from_min_size(pos2(2000.0, 2000.0), vec2(800.0, 600.0));
        layout.visible = Some(shown);
        let ops = [create("/d", "chan.constant")];
        let more = net.apply(&penumbra_ops::builtin_types().unwrap(), &ops).unwrap().network;
        layout.sync(&more, Instant::now());
        let d = layout.get(more.resolve(&"/d".parse().unwrap()).unwrap()).unwrap();
        assert!(shown.contains_rect(Rect::from_min_size(d, NODE_SIZE)), "{d:?}");
    }

    #[test]
    fn each_component_has_a_plane_of_its_own() {
        let types = penumbra_ops::builtin_types().unwrap();
        let ops = [create("/g", "comp.base"), create("/g/x", "chan.constant")];
        let net = chain().apply(&types, &ops).unwrap().network;
        let mut layout = Layout::load(None);
        layout.sync(&net, Instant::now());
        let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
        // Inside, nothing else is in the way: the first node takes the first place, which a
        // node outside holds too.
        assert_eq!(layout.get(id("/g/x")), layout.get(id("/a")));
        let outside = layout.bounds().unwrap();
        layout.level = Some(id("/g"));
        assert_eq!(layout.bounds(), Some(Rect::from_min_size(layout.get(id("/g/x")).unwrap(), NODE_SIZE)));
        assert!(outside.width() > NODE_SIZE.x);
    }

    #[test]
    fn positions_survive_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("penumbra-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let net = chain();
        let a = net.resolve(&"/a".parse().unwrap()).unwrap();
        let now = Instant::now();
        let mut layout = Layout::load(Some(&dir));
        layout.sync(&net, now);
        layout.set(a, pos2(500.0, -20.0), now);
        layout.save_if_due(&net, now, false);
        assert!(!dir.join(FILE_NAME).exists(), "not before the delay");
        layout.save_if_due(&net, now + SAVE_DELAY, false);
        let mut again = Layout::load(Some(&dir));
        again.sync(&net, now);
        assert_eq!(again.get(a), Some(pos2(500.0, -20.0)));
        std::fs::write(dir.join(FILE_NAME), "garbage").unwrap();
        let broken = Layout::load(Some(&dir));
        assert!(broken.pos.is_empty() && broken.loaded.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
