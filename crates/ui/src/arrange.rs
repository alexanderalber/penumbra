//! Arranging the nodes of one level automatically, as a layered drawing: columns by the flow of
//! data, an order within each column that keeps wires from crossing, and heights that let as many
//! wires as possible run straight.
//!
//! The steps are the usual ones for drawings of this kind (after Sugiyama):
//!
//! 1. **Columns.** A node sits right of everything it reads, wires and bindings alike. A node that
//!    feeds more than it reads moves right up to the first node it feeds, so a source lands
//!    beside its consumer rather than in the far left column.
//! 2. **Lanes for long wires.** A wire across several columns holds a thin placeholder in each
//!    column it passes, so it gets a lane of its own instead of running through other nodes.
//! 3. **Order.** Each column is sorted by the mean place of its neighbours, sweeping left to right
//!    and back, then neighbours are swapped while that removes crossings. Ports count: a wire
//!    into a node's second input lies below one into its first. The order with the fewest
//!    crossings found wins.
//! 4. **Heights.** Each node is pulled to where its wires run level with their other end; lanes
//!    go first, so a long wire stays straight, and nodes push each other aside without changing
//!    the order.
//!
//! Bindings take part in the columns only: a channel driving many params fans out whatever the
//! layout, so it gets no lanes and does not steer the order.
//!
//! Separate parts of a network are stacked top to bottom in the order they were in, single
//! unconnected nodes in rows below them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use egui::{Pos2, pos2};
use penumbra_core::NodeId;
use penumbra_graph::{Network, NodeTypes};

use crate::editor::node_ports;
use crate::layout::{GAP, Layout, NODE_SIZE};

/// The height of a long wire's lane in each column it crosses.
const LANE: f32 = 14.0;
/// Space between a lane and its neighbours in a column.
const LANE_GAP: f32 = 8.0;
/// Rounds of sweeps when ordering, and passes when setting heights.
const SWEEPS: usize = 12;
const PASSES: usize = 10;

/// A wire between two nodes of the level, with the heights of its ports below each node's top.
#[derive(Clone, Copy, Debug)]
pub struct Wire {
    pub from: NodeId,
    pub from_y: f32,
    pub to: NodeId,
    pub to_y: f32,
}

/// What the arrangement looks at: the nodes with their heights, in network order, and the wires
/// and bindings between them.
#[derive(Default, Debug)]
pub struct Graph {
    pub nodes: Vec<(NodeId, f32)>,
    pub wires: Vec<Wire>,
    /// (source, reader).
    pub binds: Vec<(NodeId, NodeId)>,
}

/// The nodes of `level` (only those in `only` when given) as the arrangement sees them. A wire
/// into or out of a component counts as one to the component, at the component's port; a
/// wire that reads the previous frame's result is left out (it runs backwards by design).
pub fn graph(
    net: &Network,
    types: &NodeTypes,
    level: Option<NodeId>,
    only: Option<&BTreeSet<NodeId>>,
) -> Graph {
    let mut g = Graph::default();
    let mut in_at: HashMap<(NodeId, u32), (NodeId, f32)> = HashMap::new();
    let mut out_at: HashMap<(NodeId, u32), (NodeId, f32)> = HashMap::new();
    for node in net.children(level).filter(|n| only.is_none_or(|s| s.contains(&n.id))) {
        let (height, ins, outs) = node_ports(net, types, node);
        in_at.extend(ins.into_iter().map(|(target, y)| (target, (node.id, y))));
        out_at.extend(outs.into_iter().map(|(target, y)| (target, (node.id, y))));
        g.nodes.push((node.id, height));
    }
    let members: HashSet<NodeId> = g.nodes.iter().map(|(id, _)| *id).collect();
    for node in net.nodes() {
        for (slot, source) in node.inputs.iter().enumerate() {
            let Some(source) = source else { continue };
            if node.is_previous(slot) {
                continue;
            }
            let Some(&(to, to_y)) = in_at.get(&(node.id, slot as u32)) else { continue };
            let Some(&(from, from_y)) = out_at.get(&(source.node, source.output)) else { continue };
            if from != to {
                g.wires.push(Wire { from, from_y, to, to_y });
            }
        }
        if node.binds.is_empty() {
            continue;
        }
        let Some(reader) = on_level(net, level, node.id).filter(|r| members.contains(r)) else { continue };
        for bind in node.binds.values() {
            if let Some(src) = on_level(net, level, bind.node).filter(|s| members.contains(s) && *s != reader)
            {
                g.binds.push((src, reader));
            }
        }
    }
    g.binds.sort();
    g.binds.dedup();
    g
}

/// The node on `level` that `id` is or sits in.
fn on_level(net: &Network, level: Option<NodeId>, id: NodeId) -> Option<NodeId> {
    let mut current = id;
    for _ in 0..=penumbra_core::NodePath::MAX_DEPTH {
        let node = net.node(current)?;
        if node.parent == level {
            return Some(current);
        }
        current = node.parent?;
    }
    None
}

/// Arranges the nodes of `level`, or only those in `only`, as one undo step. The arrangement
/// keeps the top left corner of what it arranges. Returns how many nodes moved.
pub fn level(
    net: &Network,
    types: &NodeTypes,
    layout: &mut Layout,
    level: Option<NodeId>,
    only: Option<&BTreeSet<NodeId>>,
    now: Instant,
) -> usize {
    let g = graph(net, types, level, only);
    let old: HashMap<NodeId, Pos2> =
        g.nodes.iter().filter_map(|(id, _)| Some((*id, layout.get(*id)?))).collect();
    let origin = old.values().fold(None::<Pos2>, |acc, p| Some(acc.map_or(*p, |a| a.min(*p))));
    let placed = arrange(&g, &old, origin.unwrap_or(Pos2::ZERO));
    layout.move_to(&placed, now)
}

/// Every level of the network, as separate undo steps; how many nodes moved.
pub fn everything(net: &Network, types: &NodeTypes, layout: &mut Layout, now: Instant) -> usize {
    let mut levels: Vec<Option<NodeId>> = vec![None];
    levels
        .extend(net.nodes().filter(|n| types.desc(&n.kind).is_some_and(|d| d.container)).map(|n| Some(n.id)));
    levels.into_iter().map(|l| level(net, types, layout, l, None, now)).sum()
}

/// The value at `i`, or the default for an index out of range (none is, by construction).
fn get<T: Copy + Default>(v: &[T], i: usize) -> T {
    v.get(i).copied().unwrap_or_default()
}

fn put<T>(v: &mut [T], i: usize, value: T) {
    if let Some(slot) = v.get_mut(i) {
        *slot = value;
    }
}

fn list(v: &[Vec<usize>], i: usize) -> &[usize] {
    v.get(i).map_or(&[], Vec::as_slice)
}

/// Positions for the nodes of `g`, the whole arrangement's top left at `origin`. `old` (where
/// the nodes were) decides the order of separate parts and where the ordering starts.
pub fn arrange(g: &Graph, old: &HashMap<NodeId, Pos2>, origin: Pos2) -> HashMap<NodeId, Pos2> {
    let n = g.nodes.len();
    let index: HashMap<NodeId, usize> = g.nodes.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
    let heights: Vec<f32> = g.nodes.iter().map(|(_, h)| h.max(1.0)).collect();
    let wires: Vec<(usize, f32, usize, f32)> = g
        .wires
        .iter()
        .filter_map(|w| Some((*index.get(&w.from)?, w.from_y, *index.get(&w.to)?, w.to_y)))
        .filter(|(a, _, b, _)| a != b)
        .collect();
    let binds: Vec<(usize, usize)> = g
        .binds
        .iter()
        .filter_map(|(a, b)| Some((*index.get(a)?, *index.get(b)?)))
        .filter(|(a, b)| a != b)
        .collect();

    // Connected parts.
    let mut part: Vec<usize> = (0..n).collect();
    fn root(part: &mut [usize], mut i: usize) -> usize {
        for _ in 0..part.len() {
            let up = get(part, i);
            if up == i {
                break;
            }
            put(part, i, get(part, up));
            i = up;
        }
        i
    }
    for (a, b) in wires.iter().map(|w| (w.0, w.2)).chain(binds.iter().copied()) {
        let (ra, rb) = (root(&mut part, a), root(&mut part, b));
        if ra != rb {
            put(&mut part, ra.max(rb), ra.min(rb));
        }
    }
    let mut parts: Vec<Vec<usize>> = Vec::new();
    let mut part_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let r = root(&mut part, i);
        let p = *part_of.entry(r).or_insert_with(|| {
            parts.push(Vec::new());
            parts.len() - 1
        });
        if let Some(members) = parts.get_mut(p) {
            members.push(i);
        }
    }
    let id_of = |i: usize| g.nodes.get(i).map(|(id, _)| *id);
    let old_of = |i: usize| id_of(i).and_then(|id| old.get(&id)).copied();
    // In the order they were in, top to bottom; parts not placed before after them.
    let top_of = |members: &[usize]| {
        members.iter().filter_map(|i| old_of(*i)).map(|p| p.y).fold(f32::INFINITY, f32::min)
    };
    parts.sort_by(|a, b| top_of(a).total_cmp(&top_of(b)).then(a.first().cmp(&b.first())));
    let (singles, groups): (Vec<Vec<usize>>, Vec<Vec<usize>>) = parts.into_iter().partition(|p| p.len() == 1);

    let column = NODE_SIZE.x + GAP.x;
    let mut out: HashMap<NodeId, Pos2> = HashMap::new();
    let mut y = 0.0f32;
    let mut width = 0.0f32;
    for members in groups {
        let local: HashMap<usize, usize> = members.iter().enumerate().map(|(k, i)| (*i, k)).collect();
        let part = Part {
            heights: members.iter().map(|i| get(&heights, *i)).collect(),
            old: members.iter().map(|i| old_of(*i)).collect(),
            wires: wires
                .iter()
                .filter_map(|(a, ay, b, by)| Some((*local.get(a)?, *ay, *local.get(b)?, *by)))
                .collect(),
            binds: binds.iter().filter_map(|(a, b)| Some((*local.get(a)?, *local.get(b)?))).collect(),
        };
        let (places, size) = part.arrange();
        for (i, p) in members.iter().zip(places) {
            if let Some(id) = id_of(*i) {
                out.insert(id, origin + p.to_vec2() + egui::vec2(0.0, y));
            }
        }
        y += size.1 + GAP.y * 3.0;
        width = width.max(size.0);
    }
    // Single nodes in rows, as wide as the widest part (at least six columns).
    let per_row = ((width + GAP.x) / column).floor().max(6.0) as usize;
    for row in singles.chunks(per_row) {
        let mut tallest = 0.0f32;
        for (c, i) in row.iter().filter_map(|p| p.first()).enumerate() {
            if let Some(id) = id_of(*i) {
                out.insert(id, origin + egui::vec2(c as f32 * column, y));
            }
            tallest = tallest.max(get(&heights, *i));
        }
        y += tallest + GAP.y;
    }
    out
}

/// One connected part, by local index.
struct Part {
    heights: Vec<f32>,
    old: Vec<Option<Pos2>>,
    /// (from, port height, to, port height).
    wires: Vec<(usize, f32, usize, f32)>,
    binds: Vec<(usize, usize)>,
}

/// A node or a lane in a column.
#[derive(Clone, Copy, Default)]
struct Item {
    node: Option<usize>,
    height: f32,
    column: usize,
}

/// A piece of wire between two neighbouring columns: (left item, its port height, right item,
/// its port height).
type Piece = (usize, f32, usize, f32);

impl Part {
    /// Top left corners by local index, and the width and height of the whole.
    fn arrange(&self) -> (Vec<Pos2>, (f32, f32)) {
        let n = self.heights.len();
        let columns = self.columns();
        let column_of = |i: usize| get(&columns, i);
        // Items: the nodes first (item i is node i), then the lanes.
        let mut items: Vec<Item> = (0..n)
            .map(|i| Item { node: Some(i), height: get(&self.heights, i), column: column_of(i) })
            .collect();
        let mut pieces: Vec<Piece> = Vec::new();
        // For the starting order: each lane's place between the old heights of its ends.
        let mut lane_key: HashMap<usize, (usize, usize, usize)> = HashMap::new();
        for &(a, ay, b, by) in &self.wires {
            let (ca, cb) = (column_of(a), column_of(b));
            if cb <= ca {
                continue;
            }
            let mut prev = (a, ay);
            for c in ca + 1..cb {
                let lane = items.len();
                items.push(Item { node: None, height: LANE, column: c });
                lane_key.insert(lane, (a, b, c));
                pieces.push((prev.0, prev.1, lane, LANE / 2.0));
                prev = (lane, LANE / 2.0);
            }
            pieces.push((prev.0, prev.1, b, by));
        }
        let count = columns.iter().copied().max().map_or(0, |m| m + 1);
        let graph = Layered::new(items, pieces, count, &self.binds);

        // Two starting orders, the old heights and the order of a walk along the wires; the
        // better result wins.
        let old_center =
            |i: usize| self.old.get(i).copied().flatten().map(|p| p.y + get(&self.heights, i) / 2.0);
        let by_old = |item: usize| -> f32 {
            match graph.item(item).node {
                Some(i) => old_center(i).unwrap_or(f32::MAX / 2.0 + i as f32),
                None => match lane_key.get(&item) {
                    Some(&(a, b, c)) => match (old_center(a), old_center(b)) {
                        (Some(ya), Some(yb)) => {
                            let span = column_of(b).saturating_sub(column_of(a)).max(1);
                            let t = c.saturating_sub(column_of(a)) as f32 / span as f32;
                            ya + (yb - ya) * t
                        }
                        _ => f32::MAX / 2.0,
                    },
                    None => f32::MAX / 2.0,
                },
            }
        };
        let walk = graph.walk_order();
        let starts = [graph.start(by_old), graph.start(|item| get(&walk, item))];
        let order = starts
            .into_iter()
            .map(|o| graph.order(o))
            .min_by_key(|(_, crossings)| *crossings)
            .map(|(o, _)| o)
            .unwrap_or_default();
        let tops = graph.heights(&order);

        let column = NODE_SIZE.x + GAP.x;
        let min_top = (0..n).map(|i| get(&tops, i)).fold(f32::INFINITY, f32::min);
        let min_top = if min_top.is_finite() { min_top } else { 0.0 };
        let places: Vec<Pos2> =
            (0..n).map(|i| pos2(column_of(i) as f32 * column, get(&tops, i) - min_top)).collect();
        let height = places.iter().zip(&self.heights).map(|(p, h)| p.y + h).fold(0.0f32, f32::max);
        let width = count as f32 * column - GAP.x;
        (places, (width.max(0.0), height))
    }

    /// The column of each node: right of all it reads, and a node that feeds more than it reads
    /// as far right as its first consumer allows. Edges that close a loop (possible between
    /// components) are left out.
    fn columns(&self) -> Vec<usize> {
        let n = self.heights.len();
        let mut outs: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (a, b) in self.wires.iter().map(|w| (w.0, w.2)).chain(self.binds.iter().copied()) {
            if let Some(o) = outs.get_mut(a)
                && !o.contains(&b)
            {
                o.push(b);
            }
        }
        // Depth-first, dropping edges back to a node still being walked; the reverse finishing
        // order is then an order where every kept edge points forward.
        let mut state = vec![0u8; n];
        let mut finished: Vec<usize> = Vec::with_capacity(n);
        let mut kept: Vec<Vec<usize>> = vec![Vec::new(); n];
        for start in 0..n {
            if get(&state, start) != 0 {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
            put(&mut state, start, 1);
            while let Some((v, next)) = stack.last().copied() {
                if let Some(&w) = list(&outs, v).get(next) {
                    if let Some(top) = stack.last_mut() {
                        top.1 += 1;
                    }
                    let keep = match get(&state, w) {
                        0 => {
                            put(&mut state, w, 1);
                            stack.push((w, 0));
                            true
                        }
                        2 => true,
                        _ => false,
                    };
                    if keep && let Some(k) = kept.get_mut(v) {
                        k.push(w);
                    }
                } else {
                    put(&mut state, v, 2);
                    finished.push(v);
                    stack.pop();
                }
            }
        }
        let topo: Vec<usize> = finished.into_iter().rev().collect();
        let mut column = vec![0usize; n];
        for &v in &topo {
            let c = get(&column, v) + 1;
            for &w in list(&kept, v) {
                let wanted = get(&column, w).max(c);
                put(&mut column, w, wanted);
            }
        }
        let wires_in = |v: usize| self.wires.iter().filter(|w| w.2 == v).count();
        let wires_out = |v: usize| self.wires.iter().filter(|w| w.0 == v).count();
        for &v in topo.iter().rev() {
            let Some(first) = list(&kept, v).iter().map(|w| get(&column, *w)).min() else { continue };
            let wanted = first.saturating_sub(1);
            if wanted > get(&column, v) && (wires_in(v) == 0 || wires_out(v) > wires_in(v)) {
                put(&mut column, v, wanted);
            }
        }
        let least = column.iter().copied().min().unwrap_or(0);
        column.iter().map(|c| c - least).collect()
    }
}

/// The items in columns with the pieces of wire between neighbouring columns.
struct Layered {
    items: Vec<Item>,
    pieces: Vec<Piece>,
    columns: usize,
    /// Per item, the pieces to its left and to its right.
    left: Vec<Vec<usize>>,
    right: Vec<Vec<usize>>,
    /// Per node item, the items it is bound to (either way), for nodes with no wires.
    bound: Vec<Vec<usize>>,
}

impl Layered {
    fn new(items: Vec<Item>, pieces: Vec<Piece>, columns: usize, binds: &[(usize, usize)]) -> Self {
        let mut left = vec![Vec::new(); items.len()];
        let mut right = vec![Vec::new(); items.len()];
        for (k, p) in pieces.iter().enumerate() {
            if let Some(r) = right.get_mut(p.0) {
                r.push(k);
            }
            if let Some(l) = left.get_mut(p.2) {
                l.push(k);
            }
        }
        let mut bound = vec![Vec::new(); items.len()];
        for &(a, b) in binds {
            for (x, y) in [(a, b), (b, a)] {
                if let Some(list) = bound.get_mut(x) {
                    list.push(y);
                }
            }
        }
        Self { items, pieces, columns, left, right, bound }
    }

    fn item(&self, i: usize) -> Item {
        get(&self.items, i)
    }

    /// The pieces to the left of `item` (`left`) or to its right.
    fn side(&self, item: usize, left: bool) -> impl Iterator<Item = Piece> + '_ {
        list(if left { &self.left } else { &self.right }, item).iter().map(|k| get(&self.pieces, *k))
    }

    /// Each column's items sorted by `key`.
    fn start(&self, key: impl Fn(usize) -> f32) -> Vec<Vec<usize>> {
        let mut order: Vec<Vec<usize>> = vec![Vec::new(); self.columns];
        for (i, item) in self.items.iter().enumerate() {
            if let Some(col) = order.get_mut(item.column) {
                col.push(i);
            }
        }
        for col in &mut order {
            col.sort_by(|a, b| key(*a).total_cmp(&key(*b)).then(a.cmp(b)));
        }
        order
    }

    /// A rank per item from a depth-first walk along the wires in both directions, so that what
    /// hangs together starts out together.
    fn walk_order(&self) -> Vec<f32> {
        let mut rank: Vec<Option<f32>> = vec![None; self.items.len()];
        let mut next = 0.0;
        for start in 0..self.items.len() {
            if rank.get(start).copied().flatten().is_some() || !list(&self.left, start).is_empty() {
                continue;
            }
            let mut stack = vec![start];
            while let Some(v) = stack.pop() {
                if rank.get(v).copied().flatten().is_some() {
                    continue;
                }
                put(&mut rank, v, Some(next));
                next += 1.0;
                let neighbours = self.side(v, false).map(|p| p.2).chain(self.side(v, true).map(|p| p.0));
                let mut more: Vec<usize> =
                    neighbours.filter(|w| rank.get(*w).copied().flatten().is_none()).collect();
                more.reverse();
                stack.extend(more);
            }
        }
        rank.into_iter()
            .map(|r| {
                r.unwrap_or_else(|| {
                    next += 1.0;
                    next
                })
            })
            .collect()
    }

    /// Where an item's port sits, as a place in its column: its index plus the port's share of
    /// its height, so a wire into the second input counts as lying below one into the first.
    fn at(&self, place: &[usize], item: usize, y: f32) -> f32 {
        get(place, item) as f32 + self.share(item, y)
    }

    fn share(&self, item: usize, y: f32) -> f32 {
        (y / self.item(item).height.max(1.0)).clamp(0.0, 1.0) * 0.98
    }

    fn places(&self, order: &[Vec<usize>]) -> Vec<usize> {
        let mut place = vec![0usize; self.items.len()];
        for col in order {
            for (k, item) in col.iter().enumerate() {
                put(&mut place, *item, k);
            }
        }
        place
    }

    /// Crossings between column `c` and the next.
    fn crossings_after(&self, place: &[usize], c: usize) -> usize {
        let ends: Vec<(f32, f32)> = self
            .pieces
            .iter()
            .filter(|p| self.item(p.0).column == c)
            .map(|p| (self.at(place, p.0, p.1), self.at(place, p.2, p.3)))
            .collect();
        ends.iter()
            .enumerate()
            .map(|(i, a)| ends.iter().skip(i + 1).filter(|b| (a.0 - b.0) * (a.1 - b.1) < 0.0).count())
            .sum()
    }

    fn crossings(&self, order: &[Vec<usize>]) -> usize {
        let place = self.places(order);
        (0..self.columns.saturating_sub(1)).map(|c| self.crossings_after(&place, c)).sum()
    }

    /// Sweeps from `order`; the best order seen and its crossings.
    fn order(&self, mut order: Vec<Vec<usize>>) -> (Vec<Vec<usize>>, usize) {
        let mut best = (order.clone(), self.crossings(&order));
        for round in 0..SWEEPS {
            if best.1 == 0 {
                break;
            }
            let rightwards = round % 2 == 0;
            let cols: Vec<usize> = if rightwards {
                (1..self.columns).collect()
            } else {
                (0..self.columns.saturating_sub(1)).rev().collect()
            };
            for c in cols {
                self.sort_column(&mut order, c, rightwards);
                self.transpose(&mut order, c);
            }
            let crossings = self.crossings(&order);
            if crossings < best.1 {
                best = (order.clone(), crossings);
            }
        }
        best
    }

    /// Sorts column `c` by the mean place of each item's neighbours in the column before it
    /// (`from_left`) or after it. An item without such neighbours keeps its place, or follows
    /// the nodes it is bound to.
    fn sort_column(&self, order: &mut [Vec<usize>], c: usize, from_left: bool) {
        let place = self.places(order);
        let sizes: Vec<usize> = order.iter().map(Vec::len).collect();
        let Some(col) = order.get_mut(c) else { return };
        let keys: HashMap<usize, f32> = col
            .iter()
            .map(|&item| {
                let ends: Vec<f32> = if from_left {
                    self.side(item, true).map(|p| self.at(&place, p.0, p.1) - self.share(item, p.3)).collect()
                } else {
                    self.side(item, false)
                        .map(|p| self.at(&place, p.2, p.3) - self.share(item, p.1))
                        .collect()
                };
                let key = if ends.is_empty() {
                    let bound: Vec<f32> = list(&self.bound, item)
                        .iter()
                        .map(|b| {
                            let size = get(&sizes, self.item(*b).column).max(1);
                            (get(&place, *b) as f32 + 0.5) / size as f32 * get(&sizes, c) as f32 - 0.5
                        })
                        .collect();
                    if bound.is_empty() {
                        get(&place, item) as f32
                    } else {
                        bound.iter().sum::<f32>() / bound.len() as f32
                    }
                } else {
                    ends.iter().sum::<f32>() / ends.len() as f32
                };
                (item, key)
            })
            .collect();
        let key = |i: &usize| keys.get(i).copied().unwrap_or(0.0);
        col.sort_by(|a, b| key(a).total_cmp(&key(b)).then(get(&place, *a).cmp(&get(&place, *b))));
    }

    /// Swaps neighbours in column `c` while that lowers the crossings on either side.
    fn transpose(&self, order: &mut [Vec<usize>], c: usize) {
        for _ in 0..8 {
            let mut improved = false;
            let mut place = self.places(order);
            let Some(col) = order.get_mut(c) else { return };
            for k in 0..col.len().saturating_sub(1) {
                let (a, b) = (get(col, k), get(col, k + 1));
                if self.pair_crossings(&place, b, a) < self.pair_crossings(&place, a, b) {
                    col.swap(k, k + 1);
                    put(&mut place, a, k + 1);
                    put(&mut place, b, k);
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
    }

    /// Crossings between the pieces of `upper` and those of `lower` when `upper` lies above.
    fn pair_crossings(&self, place: &[usize], upper: usize, lower: usize) -> usize {
        let mut count = 0;
        for left in [true, false] {
            let far = |item: usize| -> Vec<f32> {
                self.side(item, left)
                    .map(|p| if left { self.at(place, p.0, p.1) } else { self.at(place, p.2, p.3) })
                    .collect()
            };
            let (u, l) = (far(upper), far(lower));
            count += u.iter().map(|x| l.iter().filter(|y| *x > **y).count()).sum::<usize>();
        }
        count
    }

    /// Tops of all items for `order`: each pulled level with its neighbours, lanes first, in
    /// passes alternating from the left and from the right, the last ones looking both ways.
    fn heights(&self, order: &[Vec<usize>]) -> Vec<f32> {
        let mut top = vec![0.0f32; self.items.len()];
        for col in order {
            let mut y = 0.0;
            let mut prev: Option<usize> = None;
            for &item in col {
                if let Some(p) = prev {
                    y += self.gap(p, item);
                }
                put(&mut top, item, y);
                y += self.item(item).height;
                prev = Some(item);
            }
        }
        for pass in 0..PASSES {
            let (from_left, from_right) = match pass {
                p if p + 2 >= PASSES => (true, true),
                p if p % 2 == 0 => (true, false),
                _ => (false, true),
            };
            let cols: Vec<usize> = if from_left && !from_right {
                (1..self.columns).collect()
            } else if from_right && !from_left {
                (0..self.columns.saturating_sub(1)).rev().collect()
            } else {
                (0..self.columns).collect()
            };
            for c in cols {
                if let Some(col) = order.get(c) {
                    self.settle(col, &mut top, from_left, from_right);
                }
            }
        }
        top
    }

    fn gap(&self, a: usize, b: usize) -> f32 {
        if self.item(a).node.is_some() && self.item(b).node.is_some() { GAP.y } else { LANE_GAP }
    }

    /// Moves the items of one column towards the tops their neighbours want, by priority: lanes,
    /// then the nodes with the most wires. A moved item pushes those not yet placed aside, never
    /// one already placed, and never changes the order.
    fn settle(&self, col: &[usize], top: &mut [f32], from_left: bool, from_right: bool) {
        let wanted: Vec<Option<f32>> = col
            .iter()
            .map(|&item| {
                let mut ends: Vec<f32> = Vec::new();
                if from_left {
                    ends.extend(self.side(item, true).map(|p| get(top, p.0) + p.1 - p.3));
                }
                if from_right {
                    ends.extend(self.side(item, false).map(|p| get(top, p.2) + p.3 - p.1));
                }
                if ends.is_empty() && list(&self.left, item).is_empty() && list(&self.right, item).is_empty()
                {
                    // Only bindings: level with the first node it is bound to.
                    ends.extend(list(&self.bound, item).iter().map(|b| get(top, *b)).take(1));
                }
                (!ends.is_empty()).then(|| ends.iter().sum::<f32>() / ends.len() as f32)
            })
            .collect();
        let priority = |k: usize| -> usize {
            let item = get(col, k);
            match self.item(item).node {
                None => usize::MAX,
                Some(_) => list(&self.left, item).len() + list(&self.right, item).len(),
            }
        };
        let mut by_priority: Vec<usize> = (0..col.len()).collect();
        by_priority.sort_by(|a, b| priority(*b).cmp(&priority(*a)).then(a.cmp(b)));
        let mut fixed = vec![false; col.len()];
        // Room item `k` of the column takes down to the next one.
        let span = |k: usize| self.item(get(col, k)).height + self.gap(get(col, k), get(col, k + 1));
        let top_at = |top: &[f32], k: usize| get(top, get(col, k));
        for k in by_priority {
            let Some(want) = wanted.get(k).copied().flatten() else { continue };
            let current = top_at(top, k);
            if want < current {
                let mut floor = f32::NEG_INFINITY;
                let mut acc = 0.0;
                for j in (0..k).rev() {
                    acc += span(j);
                    if get(&fixed, j) {
                        floor = top_at(top, j) + acc;
                        break;
                    }
                }
                put(top, get(col, k), want.max(floor).min(current));
                for j in (0..k).rev() {
                    let limit = top_at(top, j + 1) - span(j);
                    if top_at(top, j) <= limit {
                        break;
                    }
                    put(top, get(col, j), limit);
                }
            } else if want > current {
                let mut ceiling = f32::INFINITY;
                let mut acc = 0.0;
                for j in k + 1..col.len() {
                    acc += span(j - 1);
                    if get(&fixed, j) {
                        ceiling = top_at(top, j) - acc;
                        break;
                    }
                }
                put(top, get(col, k), want.min(ceiling).max(current));
                for j in k + 1..col.len() {
                    let limit = top_at(top, j - 1) + span(j - 1);
                    if top_at(top, j) >= limit {
                        break;
                    }
                    put(top, get(col, j), limit);
                }
            }
            put(&mut fixed, k, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Rect;

    fn id(i: u64) -> NodeId {
        NodeId(i)
    }

    fn wire(from: u64, to: u64, to_y: f32) -> Wire {
        Wire { from: id(from), from_y: 60.0, to: id(to), to_y }
    }

    fn no_overlap(g: &Graph, at: &HashMap<NodeId, Pos2>) {
        let rects: Vec<Rect> =
            g.nodes.iter().map(|(i, h)| Rect::from_min_size(at[i], egui::vec2(NODE_SIZE.x, *h))).collect();
        for (k, a) in rects.iter().enumerate() {
            for b in &rects[k + 1..] {
                assert!(!a.intersects(*b), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn a_chain_runs_left_to_right_on_one_line() {
        let g = Graph {
            nodes: (0..4).map(|i| (id(i), 104.0)).collect(),
            wires: vec![wire(0, 1, 60.0), wire(1, 2, 60.0), wire(2, 3, 60.0)],
            binds: vec![],
        };
        let at = arrange(&g, &HashMap::new(), pos2(10.0, 20.0));
        for i in 1..4 {
            assert!(at[&id(i)].x > at[&id(i - 1)].x);
            assert_eq!(at[&id(i)].y, at[&id(i - 1)].y);
        }
        assert_eq!(at[&id(0)], pos2(10.0, 20.0));
    }

    #[test]
    fn crossed_inputs_are_untangled() {
        // a and b feed c's inputs crosswise when a starts above b.
        let g = Graph {
            nodes: vec![(id(0), 104.0), (id(1), 104.0), (id(2), 104.0)],
            wires: vec![wire(0, 2, 70.0), wire(1, 2, 40.0)],
            binds: vec![],
        };
        let old: HashMap<NodeId, Pos2> =
            [(id(0), pos2(0.0, 0.0)), (id(1), pos2(0.0, 200.0)), (id(2), pos2(200.0, 0.0))].into();
        let at = arrange(&g, &old, Pos2::ZERO);
        assert!(at[&id(1)].y < at[&id(0)].y, "the source of the upper input sits above");
        no_overlap(&g, &at);
    }

    #[test]
    fn a_long_wire_finds_a_free_corridor() {
        // 0 -> 1 -> 2 -> 3 and 0 -> 3 directly: the long wire must not run through 1 and 2.
        let g = Graph {
            nodes: (0..4).map(|i| (id(i), 104.0)).collect(),
            wires: vec![wire(0, 1, 60.0), wire(1, 2, 60.0), wire(2, 3, 40.0), wire(0, 3, 80.0)],
            binds: vec![],
        };
        let at = arrange(&g, &HashMap::new(), Pos2::ZERO);
        no_overlap(&g, &at);
        let rect = |i: u64| Rect::from_min_size(at[&id(i)], egui::vec2(NODE_SIZE.x, 104.0));
        let rects: Vec<Rect> = (0..4).map(rect).collect();
        let (a, b) = (rect(0).min + egui::vec2(NODE_SIZE.x, 60.0), rect(3).min + egui::vec2(0.0, 80.0));
        let route = crate::editor::wire_route(a, b, rect(0), rect(3), &rects, 1.0);
        let clear = crate::editor::route_cost(&route, &[], 1.0);
        assert_eq!(crate::editor::route_cost(&route, &rects, 1.0), clear, "the wire crosses no node");
    }

    #[test]
    fn a_source_sits_beside_its_consumer_and_loops_do_not_hang() {
        let g = Graph {
            nodes: (0..5).map(|i| (id(i), 48.0)).collect(),
            wires: vec![
                wire(0, 1, 30.0),
                wire(1, 2, 30.0),
                wire(2, 3, 30.0),
                wire(4, 3, 20.0),
                wire(3, 1, 30.0),
            ],
            binds: vec![(id(0), id(4))],
        };
        let at = arrange(&g, &HashMap::new(), Pos2::ZERO);
        assert!(at[&id(4)].x > at[&id(0)].x);
        assert!(at[&id(4)].x < at[&id(3)].x);
        no_overlap(&g, &at);
    }

    #[test]
    fn separate_parts_and_single_nodes_do_not_overlap() {
        let g = Graph {
            nodes: (0..7).map(|i| (id(i), 104.0)).collect(),
            wires: vec![wire(0, 1, 60.0), wire(2, 3, 60.0)],
            binds: vec![],
        };
        let at = arrange(&g, &HashMap::new(), Pos2::ZERO);
        assert_eq!(at.len(), 7);
        no_overlap(&g, &at);
    }

    #[test]
    fn a_large_tangled_network_is_arranged_without_overlap() {
        // 400 nodes, each reading two earlier ones picked by a fixed pseudo-random sequence.
        let mut seed = 12345u64;
        let mut next = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let nodes: Vec<(NodeId, f32)> =
            (0..400).map(|i| (id(i), if i % 3 == 0 { 48.0 } else { 104.0 })).collect();
        let mut wires = Vec::new();
        for i in 1..400 {
            wires.push(wire(next(i), i, 30.0));
            wires.push(wire(next(i), i, 40.0));
        }
        let g = Graph { nodes, wires, binds: vec![(id(0), id(399))] };
        let started = std::time::Instant::now();
        let at = arrange(&g, &HashMap::new(), Pos2::ZERO);
        eprintln!("400 nodes arranged in {:?}", started.elapsed());
        assert_eq!(at.len(), 400);
        no_overlap(&g, &at);
    }
}
