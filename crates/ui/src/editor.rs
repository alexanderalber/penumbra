//! The node editor: nodes with live thumbnails, wires coloured by data type, cook time as tint. Own
//! code rather than `egui-snarl`: the network snapshot is the only graph, so there is nothing to
//! keep in sync, and every node, port and wire gets an automation id.
//!
//! Mouse: left-drag a node moves it (with the selection), left-drag on empty space selects by
//! box, right- or middle-drag pans, the wheel zooms, right-click on a node opens its menu (on
//! empty space it does nothing), a double-click on empty space (or Tab) the "add node" dialog
//! ([`crate::add_dialog`]), a double-click on a component enters it (on other nodes it does
//! nothing; pinning to the viewer is in the node menu). Drag from an output to an input
//! connects; dragging a connected input picks its wire up (drop on nothing to disconnect).
//! Keys: Tab adds a node, Delete deletes, F frames, L arranges ([`crate::arrange`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use egui::emath::GuiRounding as _;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use penumbra_core::{NodeId, NodePath};
use penumbra_graph::{Data, DataKind, Network, NodeDesc, NodeStatus, NodeTypes, Op};

use crate::add_dialog::{AddDialog, Outcome};
use crate::automation;
use crate::layout::{Layout, NODE_SIZE, View, ZOOM};
use crate::textures::Textures;
use crate::{Action, theme};

const HEADER: f32 = 20.0;
const PORT_R: f32 = 5.0;
/// Pick radius of a port, in points.
const PORT_PICK: f32 = 11.0;

/// What the editor needs from the app each frame.
pub struct Inputs<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    pub status: &'a std::collections::BTreeMap<NodeId, NodeStatus>,
    pub outputs: &'a std::collections::BTreeMap<NodeId, Vec<Data>>,
    /// The nodes whose picture is drawn (up to date, see `ViewFrame::pictures`); others show
    /// none rather than an outdated one.
    pub pictures: &'a BTreeSet<NodeId>,
    pub thumbnail_mode: penumbra_graph::ThumbnailMode,
    /// Cook passes between two thumbnail cooks of a node (`ViewFrame::thumbnail_every`).
    pub thumbnail_every: u32,
    /// The show clock's frame rate, to name thumbnail rates in Hz.
    pub fps: f64,
    /// The bar under a single channel's value (`EditorOptions::chan_bars`).
    pub chan_bars: bool,
    /// Every binding as a line (`EditorOptions::binding_lines`), not only those of the
    /// selected or hovered nodes.
    pub binding_lines: bool,
    /// The viewer's node and output, drawn behind the network (`EditorOptions::viewer_background`).
    pub backdrop: Option<(NodeId, u32)>,
    /// Frame period in seconds, for the cook-time tint.
    pub period: f64,
    pub can_edit: bool,
    pub viewer: Option<NodeId>,
    /// Nodes to outline: touched by the change-log entry under the pointer.
    pub highlight: &'a BTreeSet<NodeId>,
    /// Nodes touched by recent edits from Claude or the file, with the time of the edit.
    pub recent: &'a HashMap<NodeId, Instant>,
    pub now: Instant,
    /// Names of the component files on offer in the add dialog.
    pub library: &'a [crate::add_dialog::Offered],
}

enum Drag {
    None,
    Nodes,
    /// A wire from an output; `detached` is the input it was picked up from.
    FromOutput {
        from: (NodeId, u32),
        detached: Option<(NodeId, u32)>,
    },
    /// A wire from an unconnected input, looking for an output.
    FromInput {
        to: (NodeId, u32),
    },
    Box {
        start: Pos2,
    },
}

pub struct Editor {
    pub selection: BTreeSet<NodeId>,
    drag: Drag,
    add_menu: Option<AddDialog>,
    /// The family the add dialog showed last.
    add_family: String,
    /// Nodes whose thumbnail the user paused: never cooked for, shown only while current.
    pub paused: BTreeSet<NodeId>,
    frame_request: bool,
    /// The canvas rect of the last frame, for `ui.arrange` and framing from outside.
    pub canvas: Rect,
    /// The component whose inside is shown; `None` for the top level.
    pub level: Option<NodeId>,
    /// The node under the pointer in the last frame, whose binding lines show.
    hovered: Option<NodeId>,
}

/// One port on screen. A component's ports stand for its input and output nodes: `target` is
/// the node and slot a wire really connects to.
struct PortGeo {
    pos: Pos2,
    kind: DataKind,
    target: (NodeId, u32),
    name: String,
    /// Reads its source's previous result (dashed wire).
    previous: bool,
}

/// One node's geometry on screen.
struct Geo {
    id: NodeId,
    rect: Rect,
    inputs: Vec<PortGeo>,
    outputs: Vec<PortGeo>,
    comp: bool,
}

fn find_port(geos: &[Geo], is_input: bool, target: (NodeId, u32)) -> Option<&PortGeo> {
    geos.iter()
        .flat_map(|g| if is_input { g.inputs.iter() } else { g.outputs.iter() })
        .find(|p| p.target == target)
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selection: BTreeSet::new(),
            drag: Drag::None,
            add_menu: None,
            add_family: "tex".into(),
            paused: BTreeSet::new(),
            frame_request: true,
            canvas: Rect::NOTHING,
            level: None,
            hovered: None,
        }
    }
}

fn to_screen(canvas: Rect, view: View, p: Pos2) -> Pos2 {
    canvas.min + (p.to_vec2() - view.origin) * view.zoom
}

fn to_graph(canvas: Rect, view: View, p: Pos2) -> Pos2 {
    ((p - canvas.min) / view.zoom + view.origin).to_pos2()
}

/// Body height of a node in graph units: a 16:9 picture for textures, a value line otherwise,
/// tall enough for its ports (`ports`: the most on one side, for components).
fn body_height(desc: Option<&NodeDesc>, ports: usize) -> f32 {
    let Some(d) = desc else { return 28.0 };
    let ports = d.inputs.len().max(d.outputs.len()).max(ports) as f32;
    let content =
        if d.outputs.first().is_some_and(|o| matches!(o.kind, DataKind::Tex | DataKind::Geo | DataKind::Mat))
        {
            NODE_SIZE.x * 9.0 / 16.0
        } else if d.container {
            56.0
        } else {
            28.0
        };
    content.max(ports * 16.0 + 8.0)
}

fn port_y(top: f32, height: f32, i: usize, n: usize) -> f32 {
    top + height * (i as f32 + 1.0) / (n as f32 + 1.0)
}

/// A free node name for `kind` among the children of `level`: `noise1`, `noise2`, ...
/// (`time_machine1` for `tex.time_machine`).
pub fn fresh_name(net: &Network, level: Option<NodeId>, kind: &str) -> String {
    let base: String = kind
        .rsplit('.')
        .next()
        .unwrap_or(kind)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let base = match base.as_str() {
        "" => "node".to_string(),
        // `comp.base` makes `group1` (`comp1` reads like a composite).
        "base" => "group".to_string(),
        _ => base,
    };
    let taken: BTreeSet<&str> = net.children(level).map(|n| n.name.as_str()).collect();
    (1..).map(|i| format!("{base}{i}")).find(|n| !taken.contains(n.as_str())).unwrap_or(base)
}

/// The path of `level` (`/` for the top).
pub fn level_path(net: &Network, level: Option<NodeId>) -> NodePath {
    level.and_then(|l| net.path_of(l)).unwrap_or_else(NodePath::root)
}

/// The ports of a node on screen: its declared ones, or for a component its input and output
/// nodes.
fn ports_of(
    net: &Network,
    types: &NodeTypes,
    node: &penumbra_graph::NodeDef,
) -> (Vec<PortGeo>, Vec<PortGeo>) {
    let Some(d) = types.desc(&node.kind) else { return (Vec::new(), Vec::new()) };
    let port = |pos, kind, target, name: &str, previous| PortGeo {
        pos,
        kind,
        target,
        name: name.to_string(),
        previous,
    };
    if d.container {
        let (ins, outs) = net.comp_ports(types, node.id);
        let side = |ids: Vec<NodeId>| -> Vec<PortGeo> {
            ids.into_iter()
                .filter_map(|p| {
                    let n = net.node(p)?;
                    let kind = types.desc(&n.kind)?.inputs.first()?.kind;
                    Some(port(Pos2::ZERO, kind, (p, 0), n.name.as_str(), false))
                })
                .collect()
        };
        return (side(ins), side(outs));
    }
    let ins = d
        .inputs
        .iter()
        .enumerate()
        .map(|(i, s)| port(Pos2::ZERO, s.kind, (node.id, i as u32), &s.name, s.previous))
        .collect();
    let outs = d
        .outputs
        .iter()
        .enumerate()
        .map(|(o, s)| port(Pos2::ZERO, s.kind, (node.id, o as u32), &s.name, false))
        .collect();
    (ins, outs)
}

/// A node's height and the heights of its input and output ports below its top, in graph units,
/// each port by the node and slot it really is (see [`ports_of`]).
#[allow(clippy::type_complexity)]
pub(crate) fn node_ports(
    net: &Network,
    types: &NodeTypes,
    node: &penumbra_graph::NodeDef,
) -> (f32, Vec<((NodeId, u32), f32)>, Vec<((NodeId, u32), f32)>) {
    let (ins, outs) = ports_of(net, types, node);
    let body = body_height(types.desc(&node.kind), ins.len().max(outs.len()));
    let side = |ports: Vec<PortGeo>| {
        let n = ports.len();
        ports.into_iter().enumerate().map(|(i, p)| (p.target, port_y(HEADER, body, i, n))).collect()
    };
    (HEADER + body, side(ins), side(outs))
}

/// A port under the pointer: (input?, node, slot, kind).
type PortHit = (bool, NodeId, u32, DataKind);

/// The nodes of the level shown, placed on the canvas, with what wires and stubs look up.
struct Placed<'a> {
    net: &'a Network,
    level: Option<NodeId>,
    /// The path of `level`, which stubs name nodes outside from.
    here: NodePath,
    view: View,
    /// Every node on this level, in network order.
    geos: Vec<Geo>,
    geo_of: HashMap<NodeId, usize>,
    /// Where each output shown on this level sits, by the node and slot it really is.
    out_at: HashMap<(NodeId, u32), (usize, usize)>,
}

/// Bindings by where they show.
struct Bindings {
    /// (source, reader), both on this level (or inside a component on it).
    pairs: BTreeSet<(NodeId, NodeId)>,
    /// (source outside, reader): a stub above the reader.
    from_outside: BTreeSet<(NodeId, NodeId)>,
    /// The readers outside by their source: a stub below the source.
    to_outside: BTreeMap<NodeId, BTreeSet<NodeId>>,
}

impl<'a> Placed<'a> {
    fn new(
        net: &'a Network,
        types: &NodeTypes,
        level: Option<NodeId>,
        layout: &Layout,
        canvas: Rect,
    ) -> Self {
        let view = layout.view;
        let mut geos: Vec<Geo> = Vec::new();
        for node in net.children(level) {
            let Some(p) = layout.get(node.id) else { continue };
            let desc = types.desc(&node.kind);
            let (mut ins, mut outs) = ports_of(net, types, node);
            let size = vec2(NODE_SIZE.x, HEADER + body_height(desc, ins.len().max(outs.len())));
            let rect = Rect::from_min_size(to_screen(canvas, view, p), size * view.zoom);
            let body_top = rect.min.y + HEADER * view.zoom;
            let body_h = rect.max.y - body_top;
            let (n_in, n_out) = (ins.len(), outs.len());
            for (i, port) in ins.iter_mut().enumerate() {
                port.pos = pos2(rect.min.x, port_y(body_top, body_h, i, n_in));
            }
            for (i, port) in outs.iter_mut().enumerate() {
                port.pos = pos2(rect.max.x, port_y(body_top, body_h, i, n_out));
            }
            let comp = desc.is_some_and(|d| d.container);
            geos.push(Geo { id: node.id, rect, inputs: ins, outputs: outs, comp });
        }
        let geo_of = geos.iter().enumerate().map(|(i, g)| (g.id, i)).collect();
        let out_at = geos
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| g.outputs.iter().enumerate().map(move |(oi, o)| (o.target, (gi, oi))))
            .collect();
        Self { net, level, here: level_path(net, level), view, geos, geo_of, out_at }
    }

    fn geo(&self, id: NodeId) -> Option<&Geo> {
        self.geo_of.get(&id).and_then(|i| self.geos.get(*i))
    }

    /// The node on this level that `id` is or sits in; `None` for nodes outside it.
    fn shown(&self, id: NodeId) -> Option<NodeId> {
        let mut current = id;
        for _ in 0..=NodePath::MAX_DEPTH {
            let node = self.net.node(current)?;
            if node.parent == self.level {
                return Some(current);
            }
            current = node.parent?;
        }
        None
    }

    /// How a stub names a node outside: as the project file would, from this level.
    fn stub_text(&self, ids: &[NodeId]) -> String {
        let first = ids.first().and_then(|id| self.net.path_of(*id)).map(|p| self.here.write_ref(&p));
        match (first, ids.len()) {
            (None, _) => String::new(),
            (Some(t), 0 | 1) => t,
            (Some(t), n) => format!("{t} +{}", n - 1),
        }
    }

    fn stub_font(&self) -> FontId {
        FontId::proportional(11.0 * self.view.zoom.clamp(0.6, 1.2))
    }

    /// Outputs read by a wire anywhere, and the readers outside this level of each output.
    #[allow(clippy::type_complexity)]
    fn connections(&self) -> (BTreeSet<(NodeId, u32)>, BTreeMap<(NodeId, u32), Vec<NodeId>>) {
        let mut connected: BTreeSet<(NodeId, u32)> = BTreeSet::new();
        let mut outside_readers: BTreeMap<(NodeId, u32), Vec<NodeId>> = BTreeMap::new();
        for n in self.net.nodes() {
            for src in n.inputs.iter().flatten() {
                connected.insert((src.node, src.output));
                if self.shown(n.id).is_none() && self.shown(src.node).is_some() {
                    outside_readers.entry((src.node, src.output)).or_default().push(n.id);
                }
            }
        }
        (connected, outside_readers)
    }

    /// The bindings that touch this level. A binding into or out of a component shows on the
    /// component.
    fn bindings(&self) -> Bindings {
        let mut b =
            Bindings { pairs: BTreeSet::new(), from_outside: BTreeSet::new(), to_outside: BTreeMap::new() };
        for node in self.net.nodes().filter(|n| !n.binds.is_empty()) {
            let Some(reader) = self.shown(node.id) else {
                for bind in node.binds.values() {
                    if let Some(src) = self.shown(bind.node) {
                        b.to_outside.entry(src).or_default().insert(node.id);
                    }
                }
                continue;
            };
            for bind in node.binds.values() {
                match self.shown(bind.node) {
                    Some(src) if src != reader => {
                        b.pairs.insert((src, reader));
                    }
                    Some(_) => {}
                    None => {
                        b.from_outside.insert((bind.node, reader));
                    }
                }
            }
        }
        b
    }
}

impl Editor {
    /// The output a wire is being dragged from, if any (the param panel binds a chan wire
    /// dropped onto a param).
    pub fn wire_source(&self) -> Option<(NodeId, u32)> {
        match self.drag {
            Drag::FromOutput { from, .. } => Some(from),
            _ => None,
        }
    }

    /// Frames all nodes on the next frame.
    pub fn frame_all(&mut self) {
        self.frame_request = true;
    }

    /// Shows the inside of component `level` (`None`: the top level); the selection goes, the
    /// view comes back as it was left, a level not seen before is framed.
    pub fn enter(&mut self, level: Option<NodeId>, layout: &mut Layout, now: Instant) {
        if level == self.level {
            return;
        }
        self.level = level;
        self.selection.clear();
        self.add_menu = None;
        self.drag = Drag::None;
        if !layout.enter(level, now) {
            self.frame_request = true;
        }
    }

    /// The breadcrumb above the canvas: the top level and each component down to the one
    /// shown, each a button that goes there.
    fn breadcrumb(
        &mut self,
        ui: &mut Ui,
        net: &Network,
        canvas: Rect,
        layout: &mut Layout,
        auto: &mut automation::Registry,
        now: Instant,
    ) {
        let Some(level) = self.level else { return };
        let mut chain: Vec<NodeId> = vec![level];
        while let Some(parent) = chain.last().and_then(|id| net.node(*id)).and_then(|n| n.parent) {
            if chain.len() > NodePath::MAX_DEPTH {
                break;
            }
            chain.push(parent);
        }
        chain.reverse();
        let mut steps: Vec<(Option<NodeId>, String)> = vec![(None, "top".to_string())];
        steps.extend(chain.iter().filter_map(|id| Some((Some(*id), net.node(*id)?.name.to_string()))));
        let font = FontId::proportional(13.0);
        let mut x = canvas.min.x + 8.0;
        let y = canvas.min.y + 8.0;
        let painter = ui.painter_at(canvas);
        let mut go: Option<Option<NodeId>> = None;
        let count = steps.len();
        for (i, (target, text)) in steps.into_iter().enumerate() {
            let last = i + 1 == count;
            let galley = painter.layout_no_wrap(text.clone(), font.clone(), theme::TEXT);
            let rect = Rect::from_min_size(pos2(x, y), galley.size() + vec2(12.0, 6.0));
            let r = ui.interact(rect, ui.id().with(("crumb", i)), Sense::click());
            let fill = if last {
                theme::family_color("comp").gamma_multiply(0.8)
            } else if r.hovered() {
                Color32::from_gray(70)
            } else {
                Color32::from_black_alpha(170)
            };
            painter.rect_filled(rect, CornerRadius::same(3), fill);
            painter.galley(rect.min + vec2(6.0, 3.0), galley, theme::TEXT);
            let path = target.and_then(|t| net.path_of(t)).map_or("/".to_string(), |p| p.to_string());
            auto.add(format!("crumb.{path}"), rect, text);
            if r.clicked() && !last {
                go = Some(target);
            }
            x = rect.max.x + 4.0;
            if !last {
                painter.text(
                    pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    ">",
                    font.clone(),
                    theme::TEXT_WEAK,
                );
                x += 12.0;
            }
        }
        if let Some(target) = go {
            self.enter(target, layout, now);
        }
    }

    /// The visible nodes with outputs: (thumbnails to cook, paused ones).
    fn visible(
        &self,
        net: &Network,
        types: &NodeTypes,
        layout: &Layout,
        canvas: Rect,
    ) -> (Vec<NodeId>, Vec<NodeId>) {
        let view = layout.view;
        net.children(self.level)
            .filter(|n| types.desc(&n.kind).is_some_and(|d| !d.outputs.is_empty()))
            .filter(|n| {
                layout.get(n.id).is_some_and(|p| {
                    let r = Rect::from_min_size(to_screen(canvas, view, p), NODE_SIZE * view.zoom);
                    r.intersects(canvas)
                })
            })
            .map(|n| n.id)
            .partition(|id| !self.paused.contains(id))
    }

    /// Draws the editor into `ui` and handles its input. Returns the visible nodes whose
    /// thumbnails it wants cooked and the visible paused ones.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        layout: &mut Layout,
        textures: &mut Textures,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
        background: Color32,
    ) -> (Vec<NodeId>, Vec<NodeId>) {
        let net = input.net;
        let (canvas, canvas_resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.canvas = canvas;
        auto.add("canvas", canvas, "node editor");
        self.follow_network(net, canvas, layout, input.now);
        self.pan_and_zoom(ui, &canvas_resp, canvas, layout, input.now);
        let view = layout.view;
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, CornerRadius::ZERO, background);
        if let Some(key) = input.backdrop {
            draw_backdrop(&painter, canvas, key, input, textures);
        }
        draw_grid(&painter, canvas, view);

        let placed = Placed::new(net, input.types, self.level, layout, canvas);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let (connected, outside_readers) = placed.connections();
        // Wires, bindings and references, under the nodes.
        self.draw_wires(&painter, input, &placed);
        draw_outside_readers(&painter, &placed, &outside_readers);
        draw_bindings(&painter, input, &placed, &self.selection, self.hovered);
        draw_references(&painter, input, &placed);

        let geos = &placed.geos;
        let (clicked_node, hovered_port) =
            self.nodes(ui, &painter, geos, canvas, input, layout, textures, auto, actions, pointer);
        self.draw_ports(&painter, geos, hovered_port, view.zoom, &connected);
        self.start_port_drag(ui, canvas, net, geos, input.can_edit);
        self.draw_dragged_wire(&painter, geos, pointer, view.zoom);
        let released = ui.input(|i| i.pointer.primary_released());
        if released {
            self.finish_wire(net, input.types, geos, pointer, actions);
        }

        // Empty canvas: box selection, clicks, menus.
        self.box_select(ui, &painter, &canvas_resp, geos, pointer, released, actions);
        self.canvas_clicks(&canvas_resp, canvas, view, clicked_node, input, layout, auto, actions);
        if released || !ui.input(|i| i.pointer.any_down()) {
            if matches!(self.drag, Drag::Nodes) {
                layout.end_move();
            }
            self.drag = Drag::None;
        }
        if hovered_port.is_some() && input.can_edit {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }

        self.keys(ui, input, canvas, layout, actions);
        self.breadcrumb(ui, net, canvas, layout, auto, input.now);
        self.add_menu(ui, input, auto, actions);
        self.visible(net, input.types, layout, canvas)
    }

    /// Keeps up with the network: back to the top when the component shown went away (deleted,
    /// dissolved, undone), the layout synced, the selection and paused thumbnails without nodes
    /// that are gone, and a requested framing done once the canvas has a size.
    fn follow_network(&mut self, net: &Network, canvas: Rect, layout: &mut Layout, now: Instant) {
        if self.level.is_some_and(|l| net.node(l).is_none()) {
            self.enter(None, layout, now);
        }
        layout.level = self.level;
        let shown = layout.view;
        layout.visible = Some(Rect::from_min_size(shown.origin.to_pos2(), canvas.size() / shown.zoom));
        layout.sync(net, now);
        let level = self.level;
        self.selection.retain(|id| net.node(*id).is_some_and(|n| n.parent == level));
        self.paused.retain(|id| net.node(*id).is_some());

        if self.frame_request && canvas.width() > 10.0 {
            self.frame_request = false;
            if let Some(b) = layout.bounds() {
                let margin = 40.0;
                let zoom = ((canvas.width() - margin * 2.0) / b.width())
                    .min((canvas.height() - margin * 2.0) / b.height())
                    .clamp(0.25, 1.25);
                let origin = b.center().to_vec2() - canvas.size() / zoom / 2.0;
                layout.set_view(View { origin, zoom }, now);
            }
        }
    }

    /// The wires into the inputs on this level, routed around the nodes. A wire from outside
    /// the component shown (an input node's source) is a stub from the left with the source's
    /// path. The wire being picked up from an input is left out.
    fn draw_wires(&self, painter: &egui::Painter, input: &Inputs<'_>, placed: &Placed<'_>) {
        let (net, geos, zoom) = (placed.net, &placed.geos, placed.view.zoom);
        let node_rects: Vec<Rect> = geos.iter().map(|g| g.rect).collect();
        for to in geos {
            for port_in in &to.inputs {
                let (reader, slot) = port_in.target;
                let Some(source) =
                    net.node(reader).and_then(|n| n.inputs.get(slot as usize).copied().flatten())
                else {
                    continue;
                };
                if let Drag::FromOutput { detached: Some(d), .. } = self.drag
                    && d == port_in.target
                {
                    continue;
                }
                let b = port_in.pos;
                let Some((from, out)) = placed
                    .out_at
                    .get(&(source.node, source.output))
                    .and_then(|(gi, oi)| Some((geos.get(*gi)?, geos.get(*gi)?.outputs.get(*oi)?)))
                else {
                    let a = b - vec2(36.0 * zoom, 0.0);
                    let color = theme::kind_color(port_in.kind);
                    wire(painter, &[wire_points(a, b, zoom)], color, zoom, false);
                    if zoom > 0.45 {
                        let text = placed.stub_text(&[source.node]);
                        painter.text(
                            a - vec2(3.0, 0.0),
                            Align2::RIGHT_CENTER,
                            text,
                            placed.stub_font(),
                            theme::TEXT_WEAK,
                        );
                    }
                    continue;
                };
                let hot = input.highlight.contains(&reader) && input.highlight.contains(&source.node);
                let route = wire_route(out.pos, b, from.rect, to.rect, &node_rects, zoom);
                if port_in.previous {
                    // Last frame's result, usually wired back from the end of a loop: dashed.
                    wire_previous(painter, &route, theme::kind_color(out.kind), zoom, hot);
                } else {
                    wire(painter, &route, theme::kind_color(out.kind), zoom, hot);
                }
            }
        }
    }

    /// The nodes on this level that reach into the canvas: their interaction, their drawing
    /// and the automation ids of them and their ports. Returns whether a node was clicked and
    /// the port under the pointer.
    #[allow(clippy::too_many_arguments)]
    fn nodes(
        &mut self,
        ui: &Ui,
        painter: &egui::Painter,
        geos: &[Geo],
        canvas: Rect,
        input: &Inputs<'_>,
        layout: &mut Layout,
        textures: &mut Textures,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
        pointer: Option<Pos2>,
    ) -> (bool, Option<PortHit>) {
        let net = input.net;
        let mut clicked_node = false;
        let mut hovered_port: Option<PortHit> = None;
        self.hovered = None;
        for g in geos {
            if !g.rect.expand(PORT_PICK).intersects(canvas) {
                continue;
            }
            let Some(node) = net.node(g.id) else { continue };
            let path = net.path_of(g.id).map(|p| p.to_string()).unwrap_or_default();
            let resp = ui.interact(g.rect, ui.id().with(("node", g.id)), Sense::click_and_drag());
            auto.add(format!("node.{path}"), g.rect, node.kind.clone());
            if resp.hovered() {
                self.hovered = Some(g.id);
            }
            self.node_interaction(ui, &resp, g.id, input, layout, actions, &mut clicked_node);
            self.draw_node(ui, painter, g, input, textures, &path, &resp);
            for (i, port) in g.inputs.iter().enumerate() {
                let r = Rect::from_center_size(port.pos, Vec2::splat(PORT_PICK * 2.0));
                auto.add(format!("port.in.{path}.{i}"), r, format!("{:?}", port.kind));
                if pointer.is_some_and(|q| q.distance(port.pos) <= PORT_PICK) {
                    hovered_port = Some((true, port.target.0, port.target.1, port.kind));
                }
            }
            for (o, port) in g.outputs.iter().enumerate() {
                let r = Rect::from_center_size(port.pos, Vec2::splat(PORT_PICK * 2.0));
                auto.add(format!("port.out.{path}.{o}"), r, format!("{:?}", port.kind));
                if pointer.is_some_and(|q| q.distance(port.pos) <= PORT_PICK) {
                    hovered_port = Some((false, port.target.0, port.target.1, port.kind));
                }
            }
        }
        (clicked_node, hovered_port)
    }

    /// Ports draw over nodes; the one under the pointer and compatible targets light up.
    fn draw_ports(
        &self,
        painter: &egui::Painter,
        geos: &[Geo],
        hovered_port: Option<PortHit>,
        zoom: f32,
        connected: &BTreeSet<(NodeId, u32)>,
    ) {
        let dragging_kind = match self.drag {
            Drag::FromOutput { from, .. } => find_port(geos, false, from).map(|o| (true, o.kind)),
            Drag::FromInput { to } => find_port(geos, true, to).map(|o| (false, o.kind)),
            _ => None,
        };
        for g in geos {
            for (is_input, ports) in [(true, &g.inputs), (false, &g.outputs)] {
                for port in ports {
                    let hovered = hovered_port.is_some_and(|h| h.0 == is_input && (h.1, h.2) == port.target);
                    let target = dragging_kind
                        .is_some_and(|(wants_input, k)| wants_input == is_input && k == port.kind);
                    let r = PORT_R * zoom.clamp(0.6, 1.4) * if hovered || target { 1.4 } else { 1.0 };
                    painter.circle_filled(port.pos, r, theme::kind_color(port.kind));
                    painter.circle_stroke(port.pos, r, Stroke::new(1.0, Color32::from_gray(20)));
                }
            }
            self.port_labels(painter, g, hovered_port, zoom, connected);
        }
    }

    /// Port drags start where the pointer pressed, before node drags see it. Pressing a
    /// connected input picks its wire up.
    fn start_port_drag(&mut self, ui: &Ui, canvas: Rect, net: &Network, geos: &[Geo], can_edit: bool) {
        let pressed_at = ui.input(|i| {
            if i.pointer.button_pressed(PointerButton::Primary) { i.pointer.press_origin() } else { None }
        });
        if let Some(at) = pressed_at
            && canvas.contains(at)
            && let Some(port) = nearest_port(geos, at)
            && can_edit
        {
            let (is_input, id, index) = port;
            self.drag = if is_input {
                match net.node(id).and_then(|n| n.inputs.get(index as usize).copied().flatten()) {
                    Some(src) => {
                        Drag::FromOutput { from: (src.node, src.output), detached: Some((id, index)) }
                    }
                    None => Drag::FromInput { to: (id, index) },
                }
            } else {
                Drag::FromOutput { from: (id, index), detached: None }
            };
        }
    }

    /// The wire being dragged, from its port to the pointer.
    fn draw_dragged_wire(&self, painter: &egui::Painter, geos: &[Geo], pointer: Option<Pos2>, zoom: f32) {
        let Some(q) = pointer else { return };
        match self.drag {
            Drag::FromOutput { from, .. } => {
                if let Some(o) = find_port(geos, false, from) {
                    let color = theme::kind_color(o.kind);
                    wire(painter, &[wire_points(o.pos, q, zoom)], color, zoom, true);
                }
            }
            Drag::FromInput { to } => {
                if let Some(i) = find_port(geos, true, to) {
                    let color = theme::kind_color(i.kind);
                    wire(painter, &[wire_points(q, i.pos, zoom)], color, zoom, true);
                }
            }
            _ => {}
        }
    }

    /// Selecting by a box dragged on empty canvas; Shift or Ctrl adds to the selection.
    #[allow(clippy::too_many_arguments)]
    fn box_select(
        &mut self,
        ui: &Ui,
        painter: &egui::Painter,
        resp: &egui::Response,
        geos: &[Geo],
        pointer: Option<Pos2>,
        released: bool,
        actions: &mut Vec<Action>,
    ) {
        if resp.drag_started_by(PointerButton::Primary)
            && matches!(self.drag, Drag::None)
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.drag = Drag::Box { start: p };
        }
        if let Drag::Box { start } = self.drag
            && let Some(q) = pointer
        {
            let r = Rect::from_two_pos(start, q);
            painter.rect(
                r,
                CornerRadius::same(2),
                theme::ACCENT.gamma_multiply(0.12),
                Stroke::new(1.0, theme::ACCENT),
                StrokeKind::Inside,
            );
            if released {
                let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                if !add {
                    self.selection.clear();
                }
                for g in geos {
                    if r.intersects(g.rect) {
                        self.selection.insert(g.id);
                    }
                }
                self.drag = Drag::None;
                actions.push(Action::Selected);
            }
        }
    }

    /// Clicks on empty canvas: a click clears the selection, a double-click opens the add
    /// dialog, a right-click the editor's options.
    #[allow(clippy::too_many_arguments)]
    fn canvas_clicks(
        &mut self,
        resp: &egui::Response,
        canvas: Rect,
        view: View,
        clicked_node: bool,
        input: &Inputs<'_>,
        layout: &mut Layout,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if resp.clicked() && !clicked_node {
            self.selection.clear();
            actions.push(Action::Selected);
        }
        // A fast double-click right after another click counts as a triple in egui.
        let double = resp.double_clicked() || resp.triple_clicked();
        if double
            && let Some(p) = resp.interact_pointer_pos()
            && input.can_edit
        {
            self.open_add_menu(input, p, to_graph(canvas, view, p));
        }
        // A right-click on empty space: the editor's options (it did nothing before, and the
        // thumbnail switch sat in the status bar).
        let mut arrange = false;
        let selected = self.selection.len();
        resp.context_menu(|ui| arrange = options_menu(ui, input, layout, auto, actions, selected));
        if arrange {
            self.arrange(input, layout);
        }
    }

    /// Arranges the selection when it holds two nodes or more, else everything shown (and frames
    /// it), as one undo step.
    fn arrange(&mut self, input: &Inputs<'_>, layout: &mut Layout) {
        let only = (self.selection.len() > 1).then_some(&self.selection);
        crate::arrange::level(input.net, input.types, layout, self.level, only, input.now);
        if only.is_none() {
            self.frame_all();
        }
    }

    fn open_add_menu(&mut self, input: &Inputs<'_>, screen: Pos2, at: Pos2) {
        // The family of the selected node, else the one used last.
        let family = match self.selection.iter().collect::<Vec<_>>().as_slice() {
            [only] => input
                .net
                .node(**only)
                .and_then(|n| n.kind.split_once('.').map(|(f, _)| f.to_string()))
                .unwrap_or_else(|| self.add_family.clone()),
            _ => self.add_family.clone(),
        };
        self.add_menu = Some(AddDialog::new(screen, at, &family));
    }

    fn pan_and_zoom(
        &mut self,
        ui: &Ui,
        resp: &egui::Response,
        canvas: Rect,
        layout: &mut Layout,
        now: Instant,
    ) {
        let mut view = layout.view;
        if resp.dragged_by(PointerButton::Secondary) || resp.dragged_by(PointerButton::Middle) {
            view.origin -= resp.drag_delta() / view.zoom;
        }
        if let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && canvas.contains(p)
            && ui.ui_contains_pointer()
        {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta().y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let before = to_graph(canvas, view, p);
                view.zoom = (view.zoom * factor).clamp(ZOOM.0, ZOOM.1);
                // Keep the graph point under the pointer in place.
                view.origin = before.to_vec2() - (p - canvas.min) / view.zoom;
            }
        }
        layout.set_view(view, now);
    }

    #[allow(clippy::too_many_arguments)]
    fn node_interaction(
        &mut self,
        ui: &Ui,
        resp: &egui::Response,
        id: NodeId,
        input: &Inputs<'_>,
        layout: &mut Layout,
        actions: &mut Vec<Action>,
        clicked_node: &mut bool,
    ) {
        // A press on a port has already started a wire drag (see `start_port_drag`), so this only
        // starts for presses elsewhere on the node.
        if resp.drag_started_by(PointerButton::Primary) && matches!(self.drag, Drag::None) {
            if !self.selection.contains(&id) {
                self.selection.clear();
                self.selection.insert(id);
                actions.push(Action::Selected);
            }
            self.drag = Drag::Nodes;
            layout.begin_move(self.selection.iter().copied());
        }
        if resp.dragged_by(PointerButton::Primary) && matches!(self.drag, Drag::Nodes) {
            let delta = resp.drag_delta() / layout.view.zoom;
            for sel in &self.selection {
                if let Some(p) = layout.get(*sel) {
                    layout.set(*sel, p + delta, input.now);
                }
            }
        }
        if resp.clicked() {
            *clicked_node = true;
            let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
            if toggle {
                if !self.selection.remove(&id) {
                    self.selection.insert(id);
                }
            } else {
                self.selection.clear();
                self.selection.insert(id);
            }
            actions.push(Action::Selected);
        }
        let net = input.net;
        let is_comp = net.node(id).and_then(|n| input.types.desc(&n.kind)).is_some_and(|d| d.container);
        // A double-click enters a component and does nothing on other nodes (pinning by
        // double-click happened by accident; it stays in the node menu).
        if resp.double_clicked() && is_comp {
            self.enter(Some(id), layout, input.now);
        }
        resp.context_menu(|ui| {
            let Some(node) = net.node(id) else { return };
            let path = net.path_of(id);
            if is_comp {
                if ui.button("Enter (I)").clicked() {
                    self.enter(Some(id), layout, input.now);
                    ui.close();
                }
            } else if ui.button("View (pin to viewer)").clicked() {
                actions.push(Action::Pin(Some(id)));
                ui.close();
            }
            // Thumbnail and lock are about outputs; a component has none of its own.
            let has_outputs = input.types.desc(&node.kind).is_some_and(|d| !d.outputs.is_empty());
            let paused = self.paused.contains(&id);
            let label = if paused { "Resume thumbnail" } else { "Pause thumbnail" };
            let tip = "A paused thumbnail is not cooked for; it shows a picture only while it is up to date";
            if has_outputs && ui.button(label).on_hover_text(tip).clicked() {
                if !self.paused.remove(&id) {
                    self.paused.insert(id);
                }
                ui.close();
            }
            ui.add_enabled_ui(input.can_edit, |ui| {
                if let Some(path) = path {
                    let lock = if node.locked { "Unlock" } else { "Lock (hold the output)" };
                    if has_outputs && ui.button(lock).clicked() {
                        actions
                            .push(Action::edit(vec![Op::Lock { path: path.clone(), locked: !node.locked }]));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        actions.push(Action::edit(vec![Op::Delete { path }]));
                        ui.close();
                    }
                }
                ui.separator();
                let group: Vec<NodeId> = if self.selection.contains(&id) {
                    self.selection.iter().copied().collect()
                } else {
                    vec![id]
                };
                if ui.button("Cut (Ctrl+X)").clicked() {
                    actions.push(Action::Cut(group.clone()));
                    ui.close();
                }
                if ui.button("Group into a component (Ctrl+G)").clicked() {
                    actions.push(Action::Group(group));
                    ui.close();
                }
                if is_comp && ui.button("Dissolve the component").clicked() {
                    actions.push(Action::Dissolve(id));
                    ui.close();
                }
            });
            let copy: Vec<NodeId> = if self.selection.contains(&id) {
                self.selection.iter().copied().collect()
            } else {
                vec![id]
            };
            if ui.button("Copy (Ctrl+C)").clicked() {
                actions.push(Action::Copy(copy));
                ui.close();
            }
            if is_comp {
                let tip = "Write it to the component library; add it to any project from the comp tab";
                if ui.button("Save to the library").on_hover_text(tip).clicked() {
                    actions.push(Action::SaveComponent(id));
                    ui.close();
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_node(
        &mut self,
        ui: &Ui,
        painter: &egui::Painter,
        g: &Geo,
        input: &Inputs<'_>,
        textures: &mut Textures,
        path: &str,
        resp: &egui::Response,
    ) {
        let Some(node) = input.net.node(g.id) else { return };
        let zoom = (g.rect.width() / NODE_SIZE.x).max(0.01);
        let status = input.status.get(&g.id);
        let load = status.map_or(0.0, |s| s.stats.average.as_secs_f64() / input.period.max(1e-9));
        let family = node.kind.split('.').next().unwrap_or("");
        let radius = CornerRadius::same((4.0 * zoom).round().clamp(0.0, 255.0) as u8);
        let body = theme::load_tint(load);
        painter.rect_filled(g.rect, radius, body);
        let header = Rect::from_min_max(g.rect.min, pos2(g.rect.max.x, g.rect.min.y + HEADER * zoom));
        painter.rect_filled(
            header,
            CornerRadius { nw: radius.nw, ne: radius.ne, sw: 0, se: 0 },
            theme::family_color(family),
        );
        let body_rect = Rect::from_min_max(pos2(g.rect.min.x, header.max.y), g.rect.max);

        // Content: a thumbnail for textures, the value for channels.
        let shown = input.pictures.contains(&g.id).then(|| input.outputs.get(&g.id)).flatten();
        let has_outputs = input.types.desc(&node.kind).is_some_and(|d| !d.outputs.is_empty());
        if shown.is_none()
            && has_outputs
            && input.thumbnail_mode != penumbra_graph::ThumbnailMode::Never
            && zoom > 0.45
        {
            // No picture rather than an outdated one.
            painter.text(
                body_rect.center(),
                Align2::CENTER_CENTER,
                "not cooked",
                FontId::proportional(11.0 * zoom),
                theme::TEXT_WEAK,
            );
        }
        if let Some(first) = shown.and_then(|o| o.first()) {
            match first.kind() {
                DataKind::Tex => {
                    if let Some(tex) = first.get::<penumbra_gpu::Tex>()
                        && let Some(tid) = textures.show((g.id, 0), tex)
                    {
                        let area = body_rect.shrink(2.0 * zoom);
                        let aspect = tex.width() as f32 / tex.height().max(1) as f32;
                        let fit = fit_rect(area, aspect).round_to_pixels(painter.pixels_per_point());
                        theme::checker(painter, fit, zoom);
                        painter.image(
                            tid,
                            fit,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                DataKind::Chan => {
                    if let Some(chans) = first.get::<penumbra_core::Chans>()
                        && zoom > 0.4
                    {
                        chan_body(painter, body_rect, chans, zoom, input.chan_bars);
                    }
                }
                DataKind::Table => {
                    if let Some(table) = first.get::<penumbra_core::Table>()
                        && zoom > 0.4
                    {
                        table_body(painter, body_rect, table, zoom);
                    }
                }
                DataKind::Geo | DataKind::Mat => {
                    // The geometry's or material's picture, as a texture's thumbnail.
                    if let Some(tex) = crate::viewer::preview_of(first)
                        && let Some(tid) = textures.show((g.id, 0), tex)
                    {
                        let area = body_rect.shrink(2.0 * zoom);
                        let aspect = tex.width() as f32 / tex.height().max(1) as f32;
                        let fit = fit_rect(area, aspect).round_to_pixels(painter.pixels_per_point());
                        theme::checker(painter, fit, zoom);
                        painter.image(
                            tid,
                            fit,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                DataKind::Light => {
                    if let Some(lights) = first.get::<penumbra_gpu::Lights>()
                        && zoom > 0.4
                    {
                        light_body(painter, body_rect, lights, !g.inputs.is_empty(), zoom);
                    }
                }
                DataKind::View => {
                    if let Some(v) = first.get::<penumbra_ops::view::Viewpoint>()
                        && zoom > 0.4
                    {
                        let lines = crate::viewer::view_lines(v);
                        for (i, line) in lines.iter().take(lines_fit(body_rect, zoom)).enumerate() {
                            painter.text(
                                pos2(
                                    body_rect.min.x + 4.0 * zoom,
                                    body_rect.min.y + (3.0 + 12.0 * i as f32) * zoom,
                                ),
                                Align2::LEFT_TOP,
                                line,
                                FontId::proportional(10.0 * zoom),
                                theme::TEXT,
                            );
                        }
                    }
                }
            }
        }

        // A component: what it holds, and a double border that sets it apart.
        if g.comp {
            let inside = input.net.children(Some(g.id)).count();
            if zoom > 0.45 {
                let ports = g.inputs.len() + g.outputs.len();
                let text = if inside == ports {
                    "empty: double-click to enter".to_string()
                } else {
                    format!("{} nodes", inside - ports.min(inside))
                };
                // At the bottom, clear of the port names beside the ports.
                painter.text(
                    pos2(body_rect.center().x, body_rect.max.y - 6.0 * zoom),
                    Align2::CENTER_BOTTOM,
                    text,
                    FontId::proportional(11.0 * zoom),
                    theme::TEXT_WEAK,
                );
            }
            let border =
                Stroke::new(1.5 * zoom.clamp(0.6, 1.4), theme::family_color(family).gamma_multiply(1.6));
            painter.rect_stroke(g.rect, radius, border, StrokeKind::Inside);
            painter.rect_stroke(g.rect.shrink(4.0 * zoom), radius, border, StrokeKind::Inside);
        }

        // Header text, badges.
        if zoom > 0.35 {
            let name = node.name.as_str();
            painter.text(
                pos2(header.min.x + 6.0 * zoom, header.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(13.0 * zoom),
                Color32::WHITE,
            );
            let short = node.kind.split('.').nth(1).unwrap_or(&node.kind);
            let mut right = header.max.x - 6.0 * zoom;
            let error = status.and_then(|s| s.error.as_deref());
            let warning = status.and_then(|s| s.warning.as_deref());
            if error.is_some() || warning.is_some() {
                let c = if error.is_some() { theme::ERROR } else { theme::WARNING };
                let center = pos2(right - 6.0 * zoom, header.center().y);
                painter.circle_filled(center, 6.5 * zoom, c);
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    "!",
                    FontId::proportional(11.0 * zoom),
                    Color32::BLACK,
                );
                right -= 16.0 * zoom;
            }
            if node.locked {
                theme::lock_glyph(painter, pos2(right - 5.0 * zoom, header.center().y), zoom);
                right -= 14.0 * zoom;
            }
            if self.paused.contains(&g.id) {
                painter.text(
                    pos2(right, header.center().y),
                    Align2::RIGHT_CENTER,
                    "||",
                    FontId::monospace(10.0 * zoom),
                    theme::TEXT_WEAK,
                );
                right -= 14.0 * zoom;
            }
            painter.text(
                pos2(right, header.center().y),
                Align2::RIGHT_CENTER,
                short,
                FontId::proportional(11.0 * zoom),
                Color32::from_white_alpha(150),
            );
        }
        if let Some(s) = status
            && let Some(penumbra_graph::JobStatus::Running { progress }) = s.job
        {
            let r = Rect::from_min_size(
                pos2(body_rect.min.x, body_rect.max.y - 3.0 * zoom),
                vec2(body_rect.width() * progress.unwrap_or(0.5).clamp(0.0, 1.0), 3.0 * zoom),
            );
            painter.rect_filled(r, CornerRadius::ZERO, theme::ACCENT);
        }

        // Outline: selection, change-log highlight, recent outside edits, errors.
        let selected = self.selection.contains(&g.id);
        let recent = input.recent.get(&g.id).map(|t| input.now.saturating_duration_since(*t).as_secs_f32());
        let outline = if selected {
            Some(Stroke::new(2.0, Color32::WHITE))
        } else if input.highlight.contains(&g.id) {
            Some(Stroke::new(2.0, theme::CLAUDE))
        } else if let Some(age) = recent.filter(|a| *a < theme::RECENT_SECONDS) {
            Some(Stroke::new(2.0, theme::CLAUDE.gamma_multiply(1.0 - age / theme::RECENT_SECONDS)))
        } else if status.is_some_and(|s| s.error.is_some()) {
            Some(Stroke::new(1.5, theme::ERROR))
        } else if input.viewer == Some(g.id) {
            Some(Stroke::new(1.0, theme::TEXT_WEAK))
        } else {
            None
        };
        if let Some(s) = outline {
            painter.rect_stroke(g.rect, radius, s, StrokeKind::Outside);
        }
        if recent.is_some_and(|a| a < theme::RECENT_SECONDS) {
            ui.ctx().request_repaint();
        }

        if resp.hovered() && matches!(self.drag, Drag::None) {
            let mut tip = format!("{path}  ({})", node.kind);
            if let Some(s) = status {
                tip.push_str(&format!(
                    "\ncook {:.2} ms average, {:.0}% of a frame",
                    s.stats.average.as_secs_f64() * 1000.0,
                    load * 100.0
                ));
                if let Some(e) = &s.error {
                    tip.push_str(&format!("\nerror: {e}"));
                }
                if let Some(w) = &s.warning {
                    tip.push_str(&format!("\nwarning: {w}"));
                }
            }
            resp.clone().on_hover_text_at_pointer(tip);
        }
    }

    /// Names next to the ports: always for the inputs of a node with several (which one is
    /// which), for the unconnected outputs of a node with several, and for the port under the
    /// pointer. `connected`: outputs read by a wire.
    fn port_labels(
        &self,
        painter: &egui::Painter,
        g: &Geo,
        hovered: Option<PortHit>,
        zoom: f32,
        connected: &BTreeSet<(NodeId, u32)>,
    ) {
        let font = egui::FontId::proportional(11.0 * zoom.clamp(0.6, 1.4));
        let gap = (PORT_R + 4.0) * zoom.clamp(0.6, 1.4);
        let label = |p: Pos2, text: &str, is_input: bool| {
            let galley = painter.layout_no_wrap(text.to_string(), font.clone(), theme::TEXT_WEAK);
            let size = galley.size();
            let min = if is_input {
                pos2(p.x + gap, p.y - size.y / 2.0)
            } else {
                pos2(p.x - gap - size.x, p.y - size.y / 2.0)
            };
            let rect = Rect::from_min_size(min, size).expand2(vec2(3.0, 1.0));
            painter.rect_filled(rect, CornerRadius::same(3), Color32::from_black_alpha(170));
            painter.galley(min, galley, theme::TEXT_WEAK);
        };
        // A component's ports are named by their nodes: always worth showing.
        let always = (g.inputs.len() > 1 || g.comp) && zoom >= 0.6;
        for port in &g.inputs {
            let is_hovered = hovered.is_some_and(|h| h.0 && (h.1, h.2) == port.target);
            if !(always || is_hovered) {
                continue;
            }
            let text = if port.previous && is_hovered {
                format!("{} (last frame)", port.name)
            } else {
                port.name.clone()
            };
            label(port.pos, &text, true);
        }
        let several = g.outputs.len() > 1 && zoom >= 0.6;
        for port in &g.outputs {
            let is_hovered = hovered.is_some_and(|h| !h.0 && (h.1, h.2) == port.target);
            let open = several && !connected.contains(&port.target);
            if is_hovered || open || (g.comp && zoom >= 0.6) {
                label(port.pos, &port.name, false);
            }
        }
    }

    /// Ends a wire drag: connect, reconnect or disconnect.
    fn finish_wire(
        &mut self,
        net: &Network,
        types: &NodeTypes,
        geos: &[Geo],
        pointer: Option<Pos2>,
        actions: &mut Vec<Action>,
    ) {
        let target = pointer.and_then(|p| nearest_port(geos, p));
        let path = |id: NodeId| net.path_of(id);
        let out_kind = |id: NodeId, o: u32| {
            net.node(id)
                .and_then(|n| types.desc(&n.kind))
                .and_then(|d| d.outputs.get(o as usize))
                .map(|s| s.kind)
        };
        let in_kind = |id: NodeId, i: u32| {
            net.node(id)
                .and_then(|n| types.desc(&n.kind))
                .and_then(|d| d.inputs.get(i as usize))
                .map(|s| s.kind)
        };
        let mut ops: Vec<Op> = Vec::new();
        match self.drag {
            Drag::FromOutput { from, detached } => {
                let onto = target.filter(|t| t.0).map(|t| (t.1, t.2));
                if onto == detached && onto.is_some() {
                    return;
                }
                if let Some((node, input)) = detached
                    && let Some(to) = path(node)
                {
                    ops.push(Op::Disconnect { to, input });
                }
                if let Some((node, input)) = onto
                    && in_kind(node, input) == out_kind(from.0, from.1)
                    && let (Some(from_path), Some(to)) = (path(from.0), path(node))
                {
                    ops.push(Op::Connect { from: from_path, output: from.1, to, input });
                }
            }
            Drag::FromInput { to } => {
                if let Some((false, node, output)) = target
                    && out_kind(node, output) == in_kind(to.0, to.1)
                    && let (Some(from), Some(to_path)) = (path(node), path(to.0))
                {
                    ops.push(Op::Connect { from, output, to: to_path, input: to.1 });
                }
            }
            _ => return,
        }
        self.drag = Drag::None;
        if !ops.is_empty() {
            actions.push(Action::edit(ops));
        }
    }

    fn keys(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        canvas: Rect,
        layout: &mut Layout,
        actions: &mut Vec<Action>,
    ) {
        if ui.ctx().text_edit_focused() || self.add_menu.is_some() {
            return;
        }
        // Keys go to the pane under the pointer, as in TouchDesigner.
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p)) else {
            return;
        };
        let (delete, frame, add, all, arrange) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace),
                i.consume_key(Modifiers::NONE, Key::F),
                i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::COMMAND, Key::A),
                i.consume_key(Modifiers::NONE, Key::L),
            )
        });
        if arrange {
            self.arrange(input, layout);
        }
        // Components: Ctrl+G groups the selection, I goes into the selected component, U up
        // (TouchDesigner's keys).
        let (group, enter, up) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::G),
                i.consume_key(Modifiers::NONE, Key::I),
                i.consume_key(Modifiers::NONE, Key::U),
            )
        });
        // R starts the selection over, or every stateful node of the level shown (not an edit,
        // so also while Claude edits).
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::R)) {
            let ids: Vec<NodeId> = if self.selection.is_empty() {
                input
                    .net
                    .children(self.level)
                    .filter(|n| input.types.desc(&n.kind).is_some_and(|d| d.stateful || d.container))
                    .map(|n| n.id)
                    .collect()
            } else {
                self.selection.iter().copied().collect()
            };
            if ids.is_empty() {
                actions.push(Action::Message("nothing to reset here".into()));
            } else {
                actions.push(Action::Reset(ids));
            }
        }
        if group && input.can_edit && !self.selection.is_empty() {
            actions.push(Action::Group(self.selection.iter().copied().collect()));
        }
        // Ctrl+C, Ctrl+X and Ctrl+V arrive as events of their own, the paste with the system
        // clipboard's text.
        let clipboard: Vec<egui::Event> = ui.input(|i| {
            i.events
                .iter()
                .filter(|e| matches!(e, egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)))
                .cloned()
                .collect()
        });
        for event in clipboard {
            let selected: Vec<NodeId> = self.selection.iter().copied().collect();
            match event {
                egui::Event::Copy if !selected.is_empty() => actions.push(Action::Copy(selected)),
                egui::Event::Cut if input.can_edit && !selected.is_empty() => {
                    actions.push(Action::Cut(selected))
                }
                egui::Event::Paste(text) if input.can_edit => actions.push(Action::Paste {
                    text: Some(text),
                    level: self.level,
                    at: to_graph(canvas, layout.view, pointer),
                }),
                _ => {}
            }
        }
        if enter
            && let [only] = self.selection.iter().copied().collect::<Vec<_>>().as_slice()
            && input.net.node(*only).and_then(|n| input.types.desc(&n.kind)).is_some_and(|d| d.container)
        {
            self.enter(Some(*only), layout, input.now);
        }
        if up && let Some(level) = self.level {
            let parent = input.net.node(level).and_then(|n| n.parent);
            self.enter(parent, layout, input.now);
            // Coming out, the component just left is selected.
            self.selection = [level].into();
            actions.push(Action::Selected);
        }
        if delete && input.can_edit && !self.selection.is_empty() {
            let ops: Vec<Op> = self
                .selection
                .iter()
                .filter_map(|id| input.net.path_of(*id))
                .map(|path| Op::Delete { path })
                .collect();
            actions.push(Action::edit(ops));
        }
        if frame {
            self.frame_all();
        }
        if add {
            // Tab also moves keyboard focus; here it means "add a node" and nothing else.
            ui.ctx().memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            if input.can_edit {
                self.open_add_menu(input, pointer, to_graph(canvas, layout.view, pointer));
            }
        }
        if all {
            self.selection = input.net.children(self.level).map(|n| n.id).collect();
            actions.push(Action::Selected);
        }
    }

    fn add_menu(
        &mut self,
        ui: &mut Ui,
        input: &Inputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let Some(menu) = self.add_menu.as_mut() else { return };
        let outcome = menu.show(ui, input.types, input.library, auto);
        self.add_family.clone_from(&menu.family);
        let at = menu.at;
        match outcome {
            Outcome::Open => return,
            Outcome::Remove(name) => {
                actions.push(Action::RemoveComponent(name));
                return;
            }
            Outcome::Close => {}
            Outcome::Chosen(kind) if kind.starts_with(crate::add_dialog::LIBRARY_MARK) => {
                let name = kind.trim_start_matches(crate::add_dialog::LIBRARY_MARK).to_string();
                actions.push(Action::AddComponent { name, level: self.level, at });
            }
            Outcome::Chosen(kind) => {
                let name = fresh_name(input.net, self.level, &kind);
                if let Ok(name) = name.parse()
                    && let Ok(path) = level_path(input.net, self.level).child(name)
                {
                    let mut ops = vec![Op::Create { path: path.clone(), kind, params: Default::default() }];
                    // A single selected node with a matching output feeds the new node, which
                    // then goes right of it (placed by the layout); otherwise at the pointer.
                    let mut place = Some(at);
                    if let [only] = self.selection.iter().copied().collect::<Vec<_>>().as_slice()
                        && let Some(connect) = auto_connect(input, *only, &path, ops.first())
                    {
                        ops.push(connect);
                        place = None;
                    }
                    actions.push(Action::Edit { ops, label: String::new(), group: None, place });
                }
            }
        }
        self.add_menu = None;
    }
}

/// A connection from `source`'s first output to the new node's first input of the same kind.
fn auto_connect(input: &Inputs<'_>, source: NodeId, new: &NodePath, create: Option<&Op>) -> Option<Op> {
    let Some(Op::Create { kind, .. }) = create else { return None };
    let src = input.net.node(source)?;
    let out = input.types.desc(&src.kind)?.outputs.first()?.kind;
    let index = input.types.desc(kind)?.inputs.iter().position(|i| i.kind == out)?;
    Some(Op::Connect {
        from: input.net.path_of(source)?,
        output: 0,
        to: new.clone(),
        input: u32::try_from(index).ok()?,
    })
}

/// The port within pick distance of `p`: `(is_input, node, index)`.
fn nearest_port(geos: &[Geo], p: Pos2) -> Option<(bool, NodeId, u32)> {
    let mut best: Option<(f32, (bool, NodeId, u32))> = None;
    for g in geos {
        for (is_input, ports) in [(true, &g.inputs), (false, &g.outputs)] {
            for port in ports {
                let d = port.pos.distance(p);
                if d <= PORT_PICK && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, (is_input, port.target.0, port.target.1)));
                }
            }
        }
    }
    best.map(|(_, port)| port)
}

fn fit_rect(area: Rect, aspect: f32) -> Rect {
    if !(aspect.is_finite() && aspect > 0.0) {
        return area;
    }
    let w = area.width().min(area.height() * aspect);
    let h = w / aspect;
    Rect::from_center_size(area.center(), vec2(w, h))
}

/// The editor's options, from a right-click on empty space.
/// The editor's options; returns whether "Arrange" was clicked.
fn options_menu(
    ui: &mut Ui,
    input: &Inputs<'_>,
    layout: &mut Layout,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
    selected: usize,
) -> bool {
    use penumbra_graph::ThumbnailMode;
    let label = if selected > 1 { "Arrange selection" } else { "Arrange" };
    let r = ui.button(format!("{label}  (L)")).on_hover_text(
        "Moves the nodes so that wires cross as little as possible and run straight where they can;          the selection when two or more nodes are selected, else everything shown. Ctrl+Z puts them back",
    );
    auto.add("options.arrange", r.rect, label);
    let arrange = r.clicked();
    if arrange {
        ui.close();
    }
    ui.separator();
    let mut options = layout.options;
    let r = ui
        .checkbox(&mut options.chan_bars, "Channel bars")
        .on_hover_text("The bar under a single channel's value: green, red while negative");
    auto.add("options.chan_bars", r.rect, "Channel bars");
    let r = ui
        .checkbox(&mut options.viewer_background, "Viewer as background")
        .on_hover_text("The viewer's picture behind the network, dimmed so nodes and wires stay readable");
    auto.add("options.viewer_background", r.rect, "Viewer as background");
    let r = ui.checkbox(&mut options.binding_lines, "All binding lines").on_hover_text(
        "A line for every binding. Off, a binding shows as a short stub on the node it drives, and as a          line while its source or that node is selected or under the pointer",
    );
    auto.add("options.binding_lines", r.rect, "All binding lines");
    layout.set_options(options, input.now);
    let r = ui.button("Helpers").on_hover_text(
        "Displays, output windows, what paces the show clock, resolution, state, OSC, audio, show-machine checks",
    );
    auto.add("options.helpers", r.rect, "Helpers");
    if r.clicked() {
        actions.push(Action::OpenHelpers);
        ui.close();
    }
    ui.separator();
    ui.label(egui::RichText::new("Thumbnails").color(theme::TEXT_WEAK));
    let current = input.thumbnail_mode;
    for mode in ThumbnailMode::ALL {
        let tip = match mode {
            ThumbnailMode::Always => "every visible node shows a live picture",
            ThumbnailMode::WhenFree => {
                "nothing is cooked for thumbnails; a node shows its picture only while it is \
                 up to date (it cooks for an output or the viewer, or nothing changed)"
            }
            ThumbnailMode::Never => "no thumbnails",
        };
        let r = ui.radio(mode == current, mode.name()).on_hover_text(tip);
        auto.add(format!("thumbnails.{}", mode.name().replace(' ', "_")), r.rect, mode.name());
        if r.clicked() {
            if mode != current {
                actions.push(Action::Thumbnails(mode));
            }
            ui.close();
        }
    }
    // How often a live thumbnail cooks: every frame (the default) is smooth, every 6th spares the
    // GPU in large networks.
    ui.add_enabled_ui(current == ThumbnailMode::Always, |ui| {
        ui.label(egui::RichText::new("Thumbnail rate").color(theme::TEXT_WEAK));
        for every in THUMBNAIL_EVERY {
            let hz = input.fps / f64::from(every);
            let label = if every == 1 { format!("{hz:.0} Hz, every frame") } else { format!("{hz:.0} Hz") };
            let r = ui.radio(every == input.thumbnail_every, label).on_hover_text(if every == 1 {
                "a live thumbnail cooks every frame".to_string()
            } else {
                format!("a live thumbnail cooks once in {every} frames at most")
            });
            auto.add(format!("thumbnails.every_{every}"), r.rect, format!("{hz:.0} Hz"));
            if r.clicked() {
                if every != input.thumbnail_every {
                    actions.push(Action::ThumbnailEvery(every));
                }
                ui.close();
            }
        }
    });
    arrange
}

/// The thumbnail rates the options menu offers, as frames between two cooks of a node.
const THUMBNAIL_EVERY: [u32; 4] = [1, 2, 3, 6];

/// How bright the viewer's picture behind the network is drawn: dimmed, or bright pictures
/// would swallow the wires.
const BACKDROP_TINT: Color32 = Color32::from_gray(150);

/// The viewer's picture (a texture, a geometry's or material's preview) behind the network,
/// as large as fits the canvas. Draws nothing for other data or before the first cook.
fn draw_backdrop(
    painter: &egui::Painter,
    canvas: Rect,
    (id, output): (NodeId, u32),
    input: &Inputs<'_>,
    textures: &mut Textures,
) {
    let Some(outputs) = input.outputs.get(&id) else { return };
    let Some(data) = outputs.get(output as usize).or_else(|| outputs.first()) else { return };
    let tex = match data.kind() {
        DataKind::Tex => data.get::<penumbra_gpu::Tex>(),
        _ => crate::viewer::preview_of(data),
    };
    // The viewer's key: the same picture shares one display copy.
    let Some(tex) = tex else { return };
    let Some(tid) = textures.show((id, output), tex) else { return };
    let aspect = tex.width() as f32 / tex.height().max(1) as f32;
    let fit = fit_rect(canvas, aspect).round_to_pixels(painter.pixels_per_point());
    painter.image(tid, fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), BACKDROP_TINT);
}

/// How many lines of 12 graph units fit into `body` below a margin of 4. Counted in graph
/// units with some slack: in screen points the height of 28 (two lines exactly) came out just
/// below or above two at different zoom levels, and the second line came and went while
/// zooming.
fn lines_fit(body: Rect, zoom: f32) -> usize {
    ((body.height() / zoom.max(1e-3) - 4.0) / 12.0 + 0.01).floor().max(1.0) as usize
}

/// A chan node's body: one channel as its value with a bar (red while negative), several as a
/// short list.
fn chan_body(painter: &egui::Painter, body: Rect, chans: &penumbra_core::Chans, zoom: f32, bars: bool) {
    let channels = chans.channels();
    if let [only] = channels {
        let v = only.value();
        if bars {
            let bar = Rect::from_min_size(
                pos2(body.min.x + 8.0 * zoom, body.max.y - 7.0 * zoom),
                vec2((body.width() - 16.0 * zoom) * (v.abs().min(1.0) as f32), 3.0 * zoom),
            );
            let color = if v < 0.0 { theme::NEGATIVE } else { theme::kind_color(DataKind::Chan) };
            painter.rect_filled(bar, CornerRadius::ZERO, color);
        }
        let text = if only.name.as_str() == "value" {
            format_value(v)
        } else {
            format!("{} {}", only.name, format_value(v))
        };
        painter.text(
            pos2(body.center().x, body.min.y + 11.0 * zoom),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(12.0 * zoom),
            theme::TEXT,
        );
        return;
    }
    let line = 12.0 * zoom;
    let fit = lines_fit(body, zoom);
    let more = channels.len().saturating_sub(fit);
    let shown = if more > 0 { fit.saturating_sub(1) } else { fit };
    for (i, c) in channels.iter().take(shown).enumerate() {
        painter.text(
            pos2(body.min.x + 6.0 * zoom, body.min.y + 3.0 * zoom + i as f32 * line),
            Align2::LEFT_TOP,
            format!("{} {}", c.name, format_value(c.value())),
            FontId::monospace(10.0 * zoom),
            theme::channel_color(i),
        );
    }
    let footer = if channels.is_empty() {
        "no channels".to_string()
    } else if more > 0 {
        format!("+{} more", more + 1)
    } else {
        String::new()
    };
    if !footer.is_empty() {
        painter.text(
            pos2(body.min.x + 6.0 * zoom, body.min.y + 3.0 * zoom + shown as f32 * line),
            Align2::LEFT_TOP,
            footer,
            FontId::monospace(10.0 * zoom),
            theme::TEXT_WEAK,
        );
    }
}

/// A table node's body: its first rows, cells separated by spaces.
fn table_body(painter: &egui::Painter, body: Rect, table: &penumbra_core::Table, zoom: f32) {
    let line = 12.0 * zoom;
    let fit = lines_fit(body, zoom);
    let max_chars = ((body.width() - 8.0 * zoom) / (6.0 * zoom)).max(1.0) as usize;
    for (i, row) in table.rows().iter().take(fit).enumerate() {
        let text: String =
            row.join("  ").chars().take(max_chars).map(|c| if c.is_control() { ' ' } else { c }).collect();
        painter.text(
            pos2(body.min.x + 4.0 * zoom, body.min.y + 3.0 * zoom + i as f32 * line),
            Align2::LEFT_TOP,
            text,
            FontId::monospace(10.0 * zoom),
            theme::TEXT,
        );
    }
    if table.num_rows() == 0 {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "empty",
            FontId::proportional(11.0 * zoom),
            theme::TEXT_WEAK,
        );
    }
}

/// A swatch of each light's colour and what kind it is; only how many beside input ports
/// (`ports`), whose names take the left side.
fn light_body(painter: &egui::Painter, body: Rect, lights: &penumbra_gpu::Lights, ports: bool, zoom: f32) {
    if ports {
        let n = lights.0.len();
        painter.text(
            pos2(body.max.x - 8.0 * zoom, body.center().y),
            Align2::RIGHT_CENTER,
            format!("{n} light{}", if n == 1 { "" } else { "s" }),
            FontId::proportional(10.0 * zoom),
            theme::TEXT,
        );
        return;
    }
    let line = 12.0 * zoom;
    let fit = lines_fit(body, zoom);
    let shown = if lights.0.len() > fit { fit.saturating_sub(1) } else { fit };
    for (i, l) in lights.0.iter().take(shown).enumerate() {
        let y = body.min.y + 3.0 * zoom + i as f32 * line;
        let swatch =
            Rect::from_min_size(pos2(body.min.x + 4.0 * zoom, y + 1.0 * zoom), vec2(10.0, 10.0) * zoom);
        painter.rect_filled(swatch, CornerRadius::same(2), crate::viewer::light_swatch(l));
        painter.text(
            pos2(swatch.max.x + 4.0 * zoom, y),
            Align2::LEFT_TOP,
            crate::viewer::light_name(l),
            FontId::proportional(10.0 * zoom),
            theme::TEXT,
        );
    }
    if lights.0.len() > shown {
        painter.text(
            pos2(body.min.x + 4.0 * zoom, body.min.y + 3.0 * zoom + shown as f32 * line),
            Align2::LEFT_TOP,
            format!("{} more", lights.0.len() - shown),
            FontId::proportional(10.0 * zoom),
            theme::TEXT_WEAK,
        );
    }
}

pub fn format_value(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v.abs() >= 1000.0 { format!("{v:.1}") } else { format!("{v:.3}") }
}

/// The path of a wire: cubic segments, each given by its four control points.
type Route = Vec<[Pos2; 4]>;

/// Control points of a wire from output `a` to input `b`, blind to the nodes around it. A wire
/// running backwards (the end of a feedback loop into its start) swings below.
fn wire_points(a: Pos2, b: Pos2, zoom: f32) -> [Pos2; 4] {
    if b.x < a.x - 20.0 * zoom {
        let out = 80.0 * zoom;
        let down = 140.0 * zoom + (a.x - b.x) * 0.08;
        return [a, a + vec2(out, down), b + vec2(-out, down), b];
    }
    let dx = ((b.x - a.x).abs() * 0.5).max(30.0 * zoom);
    [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b]
}

/// The route of a wire from output `a` of node `from` to input `b` of node `to`. A forward wire
/// keeps the plain curve unless that clips a node; then it bends into a free horizontal corridor
/// between the nodes in its way, runs along it and bends out to its input. A backward wire can
/// swing below both nodes, above them, or snake through the gap between them when one sits
/// above the other, each as one curve or, when that clips a node, as a wider loop around both;
/// the one that runs through the least node area wins, then the shorter one.
pub(crate) fn wire_route(a: Pos2, b: Pos2, from: Rect, to: Rect, nodes: &[Rect], zoom: f32) -> Route {
    if b.x >= a.x - 20.0 * zoom {
        return forward_route(a, b, nodes, zoom);
    }
    let out = 80.0 * zoom;
    let margin = 30.0 * zoom;
    let below = from.max.y.max(to.max.y) + margin;
    let above = from.min.y.min(to.min.y) - margin;
    let right = from.max.x.max(to.max.x) + margin;
    let left = from.min.x.min(to.min.x) - margin;
    // Each single curve also comes swinging out wider, to clear a corner it would graze.
    let wides = [1.0, 1.25, 1.5];
    // One curve, both inner control points at one height so that its middle lands at `y`.
    let swing = |y: f32, wide: f32| {
        let c = (y - (a.y + b.y) / 8.0) / 0.75;
        vec![[a, pos2(a.x + out * wide, c), pos2(b.x - out * wide, c), b]]
    };
    let mut candidates = vec![
        (around(a, b, right, below, left, margin), 1.3),
        (around(a, b, right, above, left, margin), 1.3),
    ];
    for wide in wides {
        candidates.push((swing(below, wide), 1.0));
        candidates.push((swing(above, wide), 1.0));
    }
    let (upper, lower) = if to.center().y < from.center().y { (to, from) } else { (from, to) };
    if lower.min.y - upper.max.y > 10.0 * zoom {
        // One S crossing midway between the ports, or a loop through the middle of the gap.
        for wide in wides {
            let k = (out * 1.5).max((a.x - b.x) * 0.75) * wide;
            candidates.push((vec![[a, a + vec2(k, 0.0), b - vec2(k, 0.0), b]], 1.0));
        }
        candidates.push((around(a, b, right, (upper.max.y + lower.min.y) / 2.0, left, margin), 1.3));
    }
    candidates
        .into_iter()
        // The loops are wider and less calm than the single curves: chosen when those clip.
        .map(|(r, weight)| (route_cost(&r, nodes, zoom) * weight, r))
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map_or_else(|| vec![wire_points(a, b, zoom)], |(_, r)| r)
}

/// A forward wire: the plain curve, or when that clips a node the cheapest corridor route.
/// Corridors are tried just above and below each node between the two ends, nearest first.
fn forward_route(a: Pos2, b: Pos2, nodes: &[Rect], zoom: f32) -> Route {
    let plain = vec![wire_points(a, b, zoom)];
    // The bends need room: a wire to the neighbouring column stays a plain curve.
    let bend = 60.0 * zoom;
    if b.x - a.x < bend * 3.0 {
        return plain;
    }
    let plain_cost = route_cost(&plain, nodes, zoom);
    if plain_cost <= route_cost(&plain, &[], zoom) * 1.001 {
        return plain;
    }
    let span =
        Rect::from_min_max(pos2(a.x + bend * 0.5, f32::NEG_INFINITY), pos2(b.x - bend * 0.5, f32::INFINITY));
    let middle = (a.y + b.y) / 2.0;
    let margin = 12.0 * zoom;
    let mut heights: Vec<f32> = nodes
        .iter()
        .filter(|r| r.min.x < span.max.x && r.max.x > span.min.x)
        .flat_map(|r| [r.min.y - margin, r.max.y + margin])
        .collect();
    heights.sort_by(|x, y| (x - middle).abs().total_cmp(&(y - middle).abs()));
    heights.dedup_by(|x, y| (*x - *y).abs() < 1.0);
    heights.truncate(16);
    // An S from each end into the corridor and a straight run along it.
    let along = |y: f32| -> Route {
        let (p, q) = (pos2(a.x + bend, y), pos2(b.x - bend, y));
        let h = bend * 0.5;
        vec![
            [a, a + vec2(h, 0.0), p - vec2(h, 0.0), p],
            [p, p.lerp(q, 1.0 / 3.0), p.lerp(q, 2.0 / 3.0), q],
            [q, q + vec2(h, 0.0), b - vec2(h, 0.0), b],
        ]
    };
    heights
        .into_iter()
        .map(along)
        // A corridor route is less calm than the plain curve: it has to be clearly better.
        .map(|r| (route_cost(&r, nodes, zoom) * 1.15, r))
        .chain(std::iter::once((plain_cost, plain.clone())))
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map_or(plain, |(_, r)| r)
}

/// A loop from `a` out to the right as far as `right`, across at height `y`, back to `left` and
/// into `b` from the left: straight runs with rounded corners, so it keeps clear of whatever
/// lies inside that frame.
fn around(a: Pos2, b: Pos2, right: f32, y: f32, left: f32, radius: f32) -> Route {
    rounded(&[a, pos2(right, a.y), pos2(right, y), pos2(left, y), pos2(left, b.y), b], radius)
}

/// A path along `points`, each inner corner rounded to a quarter circle of up to `radius`.
/// The corners next to the ends may use the whole first or last run, the others half of theirs.
fn rounded(points: &[Pos2], radius: f32) -> Route {
    // Bezier handle length of a quarter circle, as a fraction of its radius.
    const ARC: f32 = 0.5523;
    let line = |p: Pos2, q: Pos2| [p, p.lerp(q, 1.0 / 3.0), p.lerp(q, 2.0 / 3.0), q];
    let last = points.len().saturating_sub(1);
    let mut route = Vec::new();
    let mut at = match points.first() {
        Some(p) => *p,
        None => return route,
    };
    for (i, w) in points.windows(3).enumerate() {
        let &[p, c, q] = w else { continue };
        let (din, dout) = ((c - p).normalized(), (q - c).normalized());
        let share_in = if i == 0 { 1.0 } else { 0.5 };
        let share_out = if i + 2 == last { 1.0 } else { 0.5 };
        let r = radius.min((c - p).length() * share_in).min((q - c).length() * share_out);
        let (s, e) = (c - din * r, c + dout * r);
        if at.distance(s) > 0.01 {
            route.push(line(at, s));
        }
        route.push([s, s + din * r * ARC, e - dout * r * ARC, e]);
        at = e;
    }
    if let Some(end) = points.last()
        && at.distance(*end) > 0.01
    {
        route.push(line(at, *end));
    }
    route
}

/// Length of a route, with the stretches inside a node counted two hundred times: any clean route
/// beats one that clips a node.
pub(crate) fn route_cost(route: &[[Pos2; 4]], nodes: &[Rect], zoom: f32) -> f32 {
    let points: Vec<Pos2> =
        route.iter().flat_map(|s| (0..=32).map(move |i| cubic_at(s, i as f32 / 32.0))).collect();
    let bounds = Rect::from_points(&points);
    // Grown, so a wire grazing a corner counts as clipping it; except right at the ports.
    let near: Vec<Rect> =
        nodes.iter().map(|r| r.expand(8.0 * zoom)).filter(|r| r.intersects(bounds)).collect();
    let ends: Vec<Pos2> = route.first().map(|s| s[0]).into_iter().chain(route.last().map(|s| s[3])).collect();
    let clips =
        |p: Pos2| ends.iter().all(|e| e.distance(p) > 12.0 * zoom) && near.iter().any(|r| r.contains(p));
    points
        .windows(2)
        .filter_map(|w| match w {
            [p, q] => {
                let len = p.distance(*q);
                let mid = p.lerp(*q, 0.5);
                Some(if clips(mid) { len * 200.0 } else { len })
            }
            _ => None,
        })
        .sum()
}

fn cubic_at(s: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let [p0, p1, p2, p3] = s.map(|p| p.to_vec2());
    (p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t)).to_pos2()
}

fn flatten(route: &[[Pos2; 4]]) -> Vec<Pos2> {
    let mut points = Vec::new();
    for s in route {
        let curve =
            egui::epaint::CubicBezierShape::from_points_stroke(*s, false, Color32::TRANSPARENT, Stroke::NONE);
        // Segments share their end points.
        let skip = usize::from(!points.is_empty());
        points.extend(curve.flatten(Some(0.5)).into_iter().skip(skip));
    }
    points
}

fn wire(painter: &egui::Painter, route: &[[Pos2; 4]], color: Color32, zoom: f32, hot: bool) {
    let width = if hot { 3.0 } else { 2.0 } * zoom.clamp(0.5, 1.5);
    let stroke = Stroke::new(width, if hot { color } else { color.gamma_multiply(0.8) });
    painter.add(Shape::line(flatten(route), stroke));
}

/// A wire into an input that reads the previous frame (`InputSpec::previous`).
fn wire_previous(painter: &egui::Painter, route: &[[Pos2; 4]], color: Color32, zoom: f32, hot: bool) {
    let width = if hot { 3.0 } else { 2.0 } * zoom.clamp(0.5, 1.5);
    let stroke = Stroke::new(width, if hot { color } else { color.gamma_multiply(0.8) });
    painter.extend(Shape::dashed_line(&flatten(route), stroke, 7.0 * zoom, 5.0 * zoom));
}

/// Outputs read outside the component shown (an output node's readers): a stub to the right
/// with the first reader's path.
fn draw_outside_readers(
    painter: &egui::Painter,
    placed: &Placed<'_>,
    outside_readers: &BTreeMap<(NodeId, u32), Vec<NodeId>>,
) {
    let zoom = placed.view.zoom;
    for g in &placed.geos {
        for port in &g.outputs {
            let Some(readers) = outside_readers.get(&port.target) else { continue };
            let a = port.pos;
            let b = a + vec2(36.0 * zoom, 0.0);
            wire(painter, &[wire_points(a, b, zoom)], theme::kind_color(port.kind), zoom, false);
            if zoom > 0.45 {
                let text = placed.stub_text(readers);
                painter.text(
                    b + vec2(3.0, 0.0),
                    Align2::LEFT_CENTER,
                    text,
                    placed.stub_font(),
                    theme::TEXT_WEAK,
                );
            }
        }
    }
}

/// A dashed line as bindings and node references draw it.
fn dashed(painter: &egui::Painter, a: Pos2, b: Pos2, color: Color32, zoom: f32) {
    let width = 1.5 * zoom.clamp(0.5, 1.5);
    painter.extend(egui::Shape::dashed_line(&[a, b], Stroke::new(width, color), 6.0 * zoom, 4.0 * zoom));
}

/// Bindings: a dashed line from the source's first chan output to the top of the node whose
/// param it drives, one per pair of nodes. Unless every line is asked for, only the lines of the
/// selected nodes and the one under the pointer are drawn: one channel often drives many nodes,
/// and its fan of lines would cover the network. The others are a short stub on the node they
/// drive. One from outside the component shown is a stub above the node, with the source's path;
/// one to a node outside a stub below the source, with the reader's path.
fn draw_bindings(
    painter: &egui::Painter,
    input: &Inputs<'_>,
    placed: &Placed<'_>,
    selection: &BTreeSet<NodeId>,
    hovered: Option<NodeId>,
) {
    let zoom = placed.view.zoom;
    let bindings = placed.bindings();
    for (src, readers) in &bindings.to_outside {
        let Some(from) = placed.geo(*src) else { continue };
        // Out of the bottom (bindings into a node come in at the top), clear of the
        // neighbours to the right.
        let a = from.rect.center_bottom();
        let b = a + vec2(0.0, 28.0 * zoom);
        let color = theme::kind_color(DataKind::Chan).gamma_multiply(0.7);
        dashed(painter, a, b, color, zoom);
        painter.circle_filled(a, 3.0 * zoom, color);
        if zoom > 0.45 {
            let readers: Vec<NodeId> = readers.iter().copied().collect();
            let text = placed.stub_text(&readers);
            painter.text(b + vec2(0.0, 2.0), Align2::CENTER_TOP, text, placed.stub_font(), theme::TEXT_WEAK);
        }
    }
    for (src, reader) in &bindings.from_outside {
        let Some(to) = placed.geo(*reader) else { continue };
        let b = pos2(to.rect.center().x, to.rect.min.y);
        let a = b - vec2(0.0, 28.0 * zoom);
        let color = theme::kind_color(DataKind::Chan).gamma_multiply(0.7);
        dashed(painter, a, b, color, zoom);
        painter.circle_filled(b, 3.0 * zoom, color);
        if zoom > 0.45 {
            let text = placed.stub_text(&[*src]);
            painter.text(
                a - vec2(0.0, 2.0),
                Align2::CENTER_BOTTOM,
                text,
                placed.stub_font(),
                theme::TEXT_WEAK,
            );
        }
    }
    for (src, reader) in bindings.pairs {
        let Some(to) = placed.geo(reader) else { continue };
        let Some(from) = placed.geo(src) else { continue };
        // A component holding the channel has no chan port: from its right edge.
        let chan_out = from.outputs.iter().find(|o| o.kind == DataKind::Chan).map(|o| o.pos);
        let Some(a) = chan_out.or_else(|| from.comp.then(|| from.rect.right_center())) else {
            continue;
        };
        let b = top_or_bottom(from.rect, to.rect);
        let hot = input.highlight.contains(&reader) && input.highlight.contains(&src);
        let touched = [src, reader].iter().any(|id| selection.contains(id) || hovered == Some(*id));
        let color = theme::kind_color(DataKind::Chan).gamma_multiply(if hot || touched { 1.0 } else { 0.7 });
        if input.binding_lines || hot || touched {
            dashed(painter, a, b, color, zoom);
        } else {
            let away = if b.y > to.rect.center().y { 1.0 } else { -1.0 };
            dashed(painter, b, b + vec2(0.0, 12.0 * zoom * away), color, zoom);
        }
        painter.circle_filled(b, 3.0 * zoom, color);
    }
}

/// Where a dashed line from `from` enters `to`: the middle of its top, or of its bottom when
/// the source lies below it.
fn top_or_bottom(from: Rect, to: Rect) -> Pos2 {
    if from.center().y > to.max.y { pos2(to.center().x, to.max.y) } else { pos2(to.center().x, to.min.y) }
}

/// Node references (tex.select, tex.feedback): a dashed line in the data kind's colour from the
/// named node's first output to the top of the node that names it.
fn draw_references(painter: &egui::Painter, input: &Inputs<'_>, placed: &Placed<'_>) {
    let (net, zoom) = (placed.net, placed.view.zoom);
    for node in net.children(placed.level) {
        let Some(to) = placed.geo(node.id) else { continue };
        let Some(desc) = input.types.desc(&node.kind) else { continue };
        for spec in desc.params.iter().filter(|s| s.node_ref) {
            let Some(text) = node.param(desc, &spec.name).and_then(|v| v.as_str()) else { continue };
            if text.trim().is_empty() {
                continue;
            }
            let Ok(src) = net.resolve_from(node.id, text) else { continue };
            let Some(from) = placed.geo(src) else { continue };
            let Some((a, kind)) = from.outputs.first().map(|o| (o.pos, o.kind)) else { continue };
            let b = top_or_bottom(from.rect, to.rect);
            let color = theme::kind_color(kind).gamma_multiply(0.7);
            dashed(painter, a, b, color, zoom);
            painter.circle_filled(b, 3.0 * zoom, color);
        }
    }
}

fn draw_grid(painter: &egui::Painter, canvas: Rect, view: View) {
    let step = 40.0 * view.zoom;
    if step < 8.0 {
        return;
    }
    let color = Color32::from_white_alpha(10);
    let start = to_graph(canvas, view, canvas.min);
    let first_x = (start.x / 40.0).floor() * 40.0;
    let first_y = (start.y / 40.0).floor() * 40.0;
    let mut x = to_screen(canvas, view, pos2(first_x, 0.0)).x;
    while x < canvas.max.x {
        painter.line_segment([pos2(x, canvas.min.y), pos2(x, canvas.max.y)], Stroke::new(1.0, color));
        x += step;
    }
    let mut y = to_screen(canvas, view, pos2(0.0, first_y)).y;
    while y < canvas.max.y {
        painter.line_segment([pos2(canvas.min.x, y), pos2(canvas.max.x, y)], Stroke::new(1.0, color));
        y += step;
    }
}
