//! Linear undo history of network snapshots.
//!
//! Every entry holds the snapshot after its edit, so undo and redo are a pointer move. Entries
//! record their source and group id; consecutive edits with the same group (one parameter drag,
//! one action by Claude over several commands) merge into one entry. An edit that changes
//! nothing records nothing.
//!
//! **Live values** ([`History::live`], OSC param edits): written into the current snapshot
//! without an entry, and remembered per param. Undo and redo write them into the snapshot they
//! step to, so stepping through the history does not roll them back. A recorded edit that
//! changes such a param takes it back into the history: the live value is written into every
//! earlier snapshot (undoing that edit returns to it) and is forgotten.
//!
//! Node positions the editor gives nodes that came without one are live the same way: placing
//! such a node is no edit of the user's, but undoing an unrelated edit must not take its place
//! away again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;

use penumbra_core::{GroupId, Name, NodeId, Value};
use serde::Serialize;

use crate::network::Network;

/// Where an edit came from; shown in the change log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Ui,
    ProjectFile,
    ControlChannel,
    /// Param edits from OSC messages.
    Osc,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub snapshot: Arc<Network>,
    pub source: Source,
    pub group: GroupId,
    pub label: String,
    /// Nodes created, changed or deleted by this entry, for highlighting in the editor.
    pub touched: BTreeSet<NodeId>,
    pub at: SystemTime,
}

/// What [`History::record`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A new entry.
    New,
    /// Merged into the entry on top (same group).
    Merged,
    /// The merged group ended where it started, so its entry was dropped.
    Collapsed,
    /// Same network as before; nothing recorded.
    NoChange,
}

#[derive(Clone, Debug)]
pub struct History {
    /// The initial state is `base`; `entries[i]` is the state after edit `i`.
    base: Arc<Network>,
    entries: Vec<Entry>,
    /// Number of entries applied: 0 means at `base`.
    cursor: usize,
    limit: usize,
    /// Params set outside the history, with their values (`None`: the default).
    live: BTreeMap<(NodeId, Name), Option<Value>>,
    /// Node positions set outside the history.
    live_pos: BTreeMap<NodeId, Option<[f32; 2]>>,
}

impl History {
    /// Default number of entries kept; older ones fold into the base.
    pub const DEFAULT_LIMIT: usize = 1000;

    pub fn new(initial: Arc<Network>) -> Self {
        Self {
            base: initial,
            entries: Vec::new(),
            cursor: 0,
            limit: Self::DEFAULT_LIMIT,
            live: BTreeMap::new(),
            live_pos: BTreeMap::new(),
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn current(&self) -> &Arc<Network> {
        match self.cursor.checked_sub(1).and_then(|i| self.entries.get(i)) {
            Some(e) => &e.snapshot,
            None => &self.base,
        }
    }

    fn current_mut(&mut self) -> &mut Arc<Network> {
        match self.cursor.checked_sub(1).and_then(|i| self.entries.get_mut(i)) {
            Some(e) => &mut e.snapshot,
            None => &mut self.base,
        }
    }

    /// Writes the live values into the current snapshot, after undo or redo.
    fn refresh(&mut self) {
        if let Some(net) = self.current().with_params(&self.live) {
            *self.current_mut() = Arc::new(net);
        }
        if let Some(net) = self.current().with_positions(&self.live_pos) {
            *self.current_mut() = Arc::new(net);
        }
    }

    /// Replaces the current snapshot without recording an entry: `snapshot` differs from it in
    /// param values and positions of `touched` only, and those are kept through undo and redo
    /// (see the module docs). Returns whether anything changed.
    pub fn live(&mut self, snapshot: Arc<Network>, touched: &BTreeSet<NodeId>) -> bool {
        let changes = self.current().param_changes(&snapshot, touched);
        let moves = self.current().pos_changes(&snapshot, touched);
        if changes.is_empty() && moves.is_empty() {
            return false;
        }
        self.live.extend(changes);
        self.live_pos.extend(moves);
        *self.current_mut() = snapshot;
        true
    }

    /// Params set live and still kept out of the history.
    pub fn live_params(&self) -> usize {
        self.live.len()
    }

    /// Before recording `snapshot`: live params it changes go back to the history.
    fn take_back(&mut self, snapshot: &Network) {
        let current = self.current().clone();
        let taken: Vec<(NodeId, Name)> = self
            .live
            .keys()
            .filter(|(id, name)| current.stored_param(*id, name) != snapshot.stored_param(*id, name))
            .cloned()
            .collect();
        let pos_of = |net: &Network, id: NodeId| net.node(id).map(|n| n.pos);
        let moved: Vec<NodeId> = self
            .live_pos
            .keys()
            .filter(|id| pos_of(&current, **id) != pos_of(snapshot, **id))
            .copied()
            .collect();
        if taken.is_empty() && moved.is_empty() {
            return;
        }
        let values: BTreeMap<(NodeId, Name), Option<Value>> =
            taken.iter().filter_map(|k| Some((k.clone(), self.live.remove(k)?))).collect();
        let places: BTreeMap<NodeId, Option<[f32; 2]>> =
            moved.iter().filter_map(|id| Some((*id, self.live_pos.remove(id)?))).collect();
        // Every snapshot up to the current one: they hold the value lazily until now.
        let upto = self.cursor;
        let fill = |net: &Arc<Network>| -> Option<Arc<Network>> {
            let with = net.with_params(&values).map(Arc::new);
            let from = with.as_ref().unwrap_or(net);
            from.with_positions(&places).map(Arc::new).or(with)
        };
        if let Some(net) = fill(&self.base) {
            self.base = net;
        }
        for e in self.entries.iter_mut().take(upto) {
            if let Some(net) = fill(&e.snapshot) {
                e.snapshot = net;
            }
        }
    }

    /// Before a new entry drops the redo branch: forgets live params of nodes that only the
    /// dropped entries had, so a node created later with the same id does not inherit them.
    fn forget_dropped(&mut self) {
        if (self.live.is_empty() && self.live_pos.is_empty()) || self.cursor == self.entries.len() {
            return;
        }
        let ids: BTreeSet<NodeId> =
            self.live.keys().map(|(id, _)| *id).chain(self.live_pos.keys().copied()).collect();
        let kept: Vec<&Arc<Network>> = std::iter::once(&self.base)
            .chain(self.entries.iter().take(self.cursor).map(|e| &e.snapshot))
            .collect();
        let gone: BTreeSet<NodeId> =
            ids.into_iter().filter(|id| !kept.iter().rev().any(|n| n.node(*id).is_some())).collect();
        self.live.retain(|(id, _), _| !gone.contains(id));
        self.live_pos.retain(|id, _| !gone.contains(id));
    }

    /// State before entry `i`.
    fn before(&self, i: usize) -> &Arc<Network> {
        match i.checked_sub(1).and_then(|j| self.entries.get(j)) {
            Some(e) => &e.snapshot,
            None => &self.base,
        }
    }

    pub fn record(
        &mut self,
        snapshot: Arc<Network>,
        source: Source,
        group: GroupId,
        label: &str,
        touched: BTreeSet<NodeId>,
    ) -> Recorded {
        if Network::same(self.current(), &snapshot) {
            return Recorded::NoChange;
        }
        if !self.live.is_empty() || !self.live_pos.is_empty() {
            self.take_back(&snapshot);
        }
        let on_top = self.cursor == self.entries.len();
        let top = self.cursor.checked_sub(1);
        if let Some(t) = top
            && on_top
            && self.entries.get(t).is_some_and(|e| e.group == group && e.source == source)
        {
            if Network::same(self.before(t), &snapshot) {
                self.entries.truncate(t);
                self.cursor = t;
                return Recorded::Collapsed;
            }
            if let Some(e) = self.entries.get_mut(t) {
                e.snapshot = snapshot;
                e.touched.extend(touched);
                e.at = SystemTime::now();
                return Recorded::Merged;
            }
        }
        self.forget_dropped();
        self.entries.truncate(self.cursor);
        self.entries.push(Entry {
            snapshot,
            source,
            group,
            label: label.to_string(),
            touched,
            at: SystemTime::now(),
        });
        self.cursor = self.entries.len();
        if self.entries.len() > self.limit {
            let drop = self.entries.len() - self.limit;
            if let Some(last_dropped) = self.entries.drain(..drop).next_back() {
                self.base = last_dropped.snapshot;
            }
            self.cursor = self.entries.len();
        }
        Recorded::New
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    /// Step back; returns the entry undone.
    pub fn undo(&mut self) -> Option<&Entry> {
        let i = self.cursor.checked_sub(1)?;
        self.cursor = i;
        self.refresh();
        self.entries.get(i)
    }

    /// Step forward; returns the entry redone.
    pub fn redo(&mut self) -> Option<&Entry> {
        let i = self.cursor;
        self.entries.get(i)?;
        self.cursor += 1;
        self.refresh();
        self.entries.get(i)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Entries currently applied (the rest can be redone).
    pub fn applied(&self) -> usize {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Op;
    use crate::testing::{create, test_types};
    use penumbra_core::{ParamPath, Value};

    fn set(net: &Network, v: f64) -> Arc<Network> {
        let op = Op::Set { param: ParamPath::parse("/a.value").unwrap(), value: Value::Float(v) };
        Arc::new(net.apply(&test_types(), &[op]).unwrap().network)
    }

    fn start() -> Arc<Network> {
        Arc::new(Network::new().apply(&test_types(), &[create("/a", "test.const")]).unwrap().network)
    }

    fn rec(h: &mut History, n: Arc<Network>, g: u64) -> Recorded {
        h.record(n, Source::Ui, GroupId(g), "edit", BTreeSet::new())
    }

    #[test]
    fn undo_redo_are_pointer_moves() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = set(&s0, 1.0);
        let s2 = set(&s1, 2.0);
        assert_eq!(rec(&mut h, s1.clone(), 1), Recorded::New);
        assert_eq!(rec(&mut h, s2.clone(), 2), Recorded::New);
        assert!(h.undo().is_some());
        assert!(Arc::ptr_eq(h.current(), &s1));
        assert!(h.undo().is_some());
        assert!(Arc::ptr_eq(h.current(), &s0));
        assert!(h.undo().is_none());
        assert!(h.redo().is_some());
        assert!(h.redo().is_some());
        assert!(h.redo().is_none());
        assert!(Arc::ptr_eq(h.current(), &s2));
    }

    #[test]
    fn no_change_records_nothing() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        // Setting the default is the same network as never setting it.
        assert_eq!(rec(&mut h, set(&s0, 0.0), 1), Recorded::NoChange);
        assert_eq!(rec(&mut h, s0.clone(), 1), Recorded::NoChange);
        assert_eq!(h.entries().len(), 0);
    }

    #[test]
    fn a_drag_is_one_entry() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let mut n = s0.clone();
        for (i, v) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            n = set(&n, v);
            assert_eq!(rec(&mut h, n.clone(), 7), if i == 0 { Recorded::New } else { Recorded::Merged });
        }
        assert_eq!(h.entries().len(), 1);
        h.undo();
        assert!(Arc::ptr_eq(h.current(), &s0));
    }

    #[test]
    fn a_drag_back_to_the_start_collapses() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = set(&s0, 0.5);
        rec(&mut h, s1.clone(), 3);
        let back = Arc::new(
            s1.apply(&test_types(), &[Op::Reset { param: "/a.value".parse().unwrap() }]).unwrap().network,
        );
        assert_eq!(rec(&mut h, back, 3), Recorded::Collapsed);
        assert!(!h.can_undo());
        assert!(Network::same(h.current(), &s0));
    }

    #[test]
    fn same_group_after_undo_does_not_merge() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = set(&s0, 1.0);
        let s2 = set(&s1, 2.0);
        rec(&mut h, s1.clone(), 1);
        rec(&mut h, s2, 1);
        h.undo();
        assert_eq!(rec(&mut h, set(&s0, 5.0), 1), Recorded::New);
        assert!(!h.can_redo());
    }

    #[test]
    fn different_sources_do_not_merge() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = set(&s0, 1.0);
        rec(&mut h, s1.clone(), 1);
        let r = h.record(set(&s1, 2.0), Source::ControlChannel, GroupId(1), "claude", BTreeSet::new());
        assert_eq!(r, Recorded::New);
    }

    #[test]
    fn new_edit_drops_redo_branch() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        rec(&mut h, set(&s0, 1.0), 1);
        rec(&mut h, set(&s0, 2.0), 2);
        h.undo();
        h.undo();
        rec(&mut h, set(&s0, 3.0), 3);
        assert_eq!(h.entries().len(), 1);
        assert!(!h.can_redo());
    }

    #[test]
    fn limit_folds_old_entries_into_base() {
        let s0 = start();
        let mut h = History::new(s0.clone()).with_limit(3);
        let mut n = s0;
        for g in 0..10 {
            n = set(&n, g as f64 + 1.0);
            rec(&mut h, n.clone(), g);
        }
        assert_eq!(h.entries().len(), 3);
        while h.undo().is_some() {}
        // Base is now the state after edit 7 (value 7).
        let a = h.current().resolve(&"/a".parse().unwrap()).unwrap();
        assert_eq!(h.current().node(a).unwrap().params.get("value"), Some(&Value::Float(7.0)));
    }

    fn value_of(net: &Network) -> Option<Value> {
        let a = net.resolve(&"/a".parse().unwrap())?;
        net.node(a)?.params.get("value").cloned()
    }

    fn live(h: &mut History, v: f64) {
        let next = set(h.current(), v);
        let a = next.resolve(&"/a".parse().unwrap()).unwrap();
        assert!(h.live(next, &BTreeSet::from([a])));
    }

    fn with_b(net: &Network) -> Arc<Network> {
        Arc::new(net.apply(&test_types(), &[create("/b", "test.const")]).unwrap().network)
    }

    #[test]
    fn live_values_survive_undo_and_redo_of_other_edits() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        rec(&mut h, set(&s0, 1.0), 1);
        let s2 = with_b(h.current());
        rec(&mut h, s2, 2);
        live(&mut h, 5.0);
        assert_eq!(h.entries().len(), 2, "no entry");
        h.undo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)));
        h.undo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)));
        assert_eq!(h.current().len(), 1);
        h.redo();
        h.redo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)));
        assert_eq!(h.current().len(), 2);
    }

    #[test]
    fn a_recorded_edit_takes_a_live_param_back() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        rec(&mut h, with_b(&s0), 1);
        live(&mut h, 5.0);
        let n = set(h.current(), 7.0);
        assert_eq!(rec(&mut h, n, 2), Recorded::New);
        assert_eq!(h.live_params(), 0);
        h.undo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)), "back to the live value");
        h.undo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)), "older snapshots got it too");
        h.redo();
        h.redo();
        assert_eq!(value_of(h.current()), Some(Value::Float(7.0)));
    }

    #[test]
    fn a_drag_through_a_live_value_undoes_to_it() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        rec(&mut h, set(&s0, 1.0), 3);
        live(&mut h, 5.0);
        let n = set(h.current(), 2.0);
        assert_eq!(rec(&mut h, n, 3), Recorded::Merged);
        h.undo();
        assert_eq!(value_of(h.current()), Some(Value::Float(5.0)));
    }

    #[test]
    fn live_values_of_dropped_nodes_are_forgotten() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = with_b(&s0);
        rec(&mut h, s1.clone(), 1);
        let b = s1.resolve(&"/b".parse().unwrap()).unwrap();
        let op = Op::Set { param: ParamPath::parse("/b.value").unwrap(), value: Value::Float(3.0) };
        let next = Arc::new(s1.apply(&test_types(), &[op]).unwrap().network);
        assert!(h.live(next, &BTreeSet::from([b])));
        h.undo();
        assert!(h.current().node(b).is_none());
        // A new node may get b's id; it must not inherit b's live value.
        let again = with_b(h.current());
        rec(&mut h, again.clone(), 2);
        assert_eq!(h.live_params(), 0);
        let b2 = again.resolve(&"/b".parse().unwrap()).unwrap();
        h.undo();
        h.redo();
        assert!(h.current().node(b2).unwrap().params.is_empty());
    }

    fn place(net: &Network, path: &str, pos: [f32; 2]) -> Arc<Network> {
        let op = Op::Place { path: path.parse().unwrap(), pos: Some(pos) };
        Arc::new(net.apply(&test_types(), &[op]).unwrap().network)
    }

    fn pos_of(net: &Network, path: &str) -> Option<[f32; 2]> {
        net.node(net.resolve(&path.parse().unwrap())?)?.pos
    }

    #[test]
    fn live_positions_survive_undo_and_a_move_undoes_to_them() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let s1 = with_b(&s0);
        rec(&mut h, s1.clone(), 1);
        let b = s1.resolve(&"/b".parse().unwrap()).unwrap();
        // The editor places the new node without an entry.
        assert!(h.live(place(&s1, "/b", [10.0, 20.0]), &BTreeSet::from([b])));
        assert_eq!(h.entries().len(), 1);
        h.undo();
        assert!(h.current().node(b).is_none());
        h.redo();
        assert_eq!(pos_of(h.current(), "/b"), Some([10.0, 20.0]), "back where it was placed");
        // A recorded move takes it back into the history: undoing the move returns to it.
        let moved = place(h.current(), "/b", [300.0, 0.0]);
        assert_eq!(rec(&mut h, moved, 2), Recorded::New);
        h.undo();
        assert_eq!(pos_of(h.current(), "/b"), Some([10.0, 20.0]));
        h.redo();
        assert_eq!(pos_of(h.current(), "/b"), Some([300.0, 0.0]));
    }

    #[test]
    fn live_changes_nothing_records_nothing() {
        let s0 = start();
        let mut h = History::new(s0.clone());
        let a = s0.resolve(&"/a".parse().unwrap()).unwrap();
        assert!(!h.live(set(&s0, 0.0), &BTreeSet::from([a])));
        assert!(Arc::ptr_eq(h.current(), &s0));
    }
}
