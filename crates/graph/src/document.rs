//! The document: the current network published through `arc-swap`, plus its history.
//!
//! Readers ([`Document::snapshot`]: the cook thread once per frame, the UI, autosave) never
//! lock. Writers (UI, control channel, project file merges) go through one mutex, apply their
//! batch to the current snapshot, record it in the history and publish the result.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use arc_swap::ArcSwap;
use penumbra_core::{Error, GroupId, NodeId};

use crate::history::{History, Recorded, Source};
use crate::network::{Network, Op, Params};
use crate::node::NodeTypes;

/// Result of [`Document::edit`].
#[derive(Clone, Debug)]
pub struct EditOutcome {
    pub recorded: Recorded,
    pub created: Vec<NodeId>,
    pub snapshot: Arc<Network>,
}

pub struct Document {
    current: ArcSwap<Network>,
    history: Mutex<History>,
    types: Arc<NodeTypes>,
    groups: AtomicU64,
}

impl Document {
    pub fn new(types: Arc<NodeTypes>, network: Network) -> Self {
        let initial = Arc::new(network);
        Self {
            current: ArcSwap::new(initial.clone()),
            history: Mutex::new(History::new(initial)),
            types,
            groups: AtomicU64::new(1),
        }
    }

    pub fn types(&self) -> &Arc<NodeTypes> {
        &self.types
    }

    /// The current network, without locking.
    pub fn snapshot(&self) -> Arc<Network> {
        self.current.load_full()
    }

    /// A fresh group id: one per parameter drag, one per action of Claude.
    pub fn new_group(&self) -> GroupId {
        GroupId(self.groups.fetch_add(1, Ordering::Relaxed))
    }

    fn lock(&self) -> MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies `ops` as one batch. An empty `label` is derived from the ops. Edits from the
    /// project file keep parameters the node types no longer declare ([`Params::Keep`]); all
    /// others refuse them.
    pub fn edit(
        &self,
        ops: &[Op],
        source: Source,
        group: GroupId,
        label: &str,
    ) -> Result<EditOutcome, Error> {
        let mut history = self.lock();
        let params = if source == Source::ProjectFile { Params::Keep } else { Params::Strict };
        let applied = history.current().apply_with(&self.types, ops, params)?;
        let label = if label.is_empty() { label_for(ops) } else { label.to_string() };
        let recorded = history.record(Arc::new(applied.network), source, group, &label, applied.touched);
        let snapshot = history.current().clone();
        self.current.store(snapshot.clone());
        Ok(EditOutcome { recorded, created: applied.created, snapshot })
    }

    /// Applies param values outside the history ([`History::live`]): no entry, and undo or redo
    /// keep them. Only [`Op::Set`] and [`Op::Reset`]; anything else is refused. Returns the
    /// published snapshot.
    pub fn edit_live(&self, ops: &[Op]) -> Result<Arc<Network>, Error> {
        if let Some(op) = ops.iter().find(|op| !matches!(op, Op::Set { .. } | Op::Reset { .. })) {
            return Err(Error::new(format!("{}: only param values can be set live", op.label())));
        }
        let mut history = self.lock();
        let applied = history.current().apply_with(&self.types, ops, Params::Strict)?;
        if history.live(Arc::new(applied.network), &applied.touched) {
            self.current.store(history.current().clone());
        }
        Ok(history.current().clone())
    }

    /// Replaces the whole network, e.g. after merging an external edit of the project file.
    pub fn replace(
        &self,
        network: Network,
        source: Source,
        group: GroupId,
        label: &str,
        touched: BTreeSet<NodeId>,
    ) -> Recorded {
        let mut history = self.lock();
        let recorded = history.record(Arc::new(network), source, group, label, touched);
        self.current.store(history.current().clone());
        recorded
    }

    /// Undo one entry; returns its label.
    pub fn undo(&self) -> Option<String> {
        let mut history = self.lock();
        let label = history.undo()?.label.clone();
        self.current.store(history.current().clone());
        Some(label)
    }

    /// Redo one entry; returns its label.
    pub fn redo(&self) -> Option<String> {
        let mut history = self.lock();
        let label = history.redo()?.label.clone();
        self.current.store(history.current().clone());
        Some(label)
    }

    pub fn mark_saved(&self) {
        self.lock().mark_saved();
    }

    pub fn is_modified(&self) -> bool {
        self.lock().is_modified()
    }

    /// Read access to the history, e.g. for the change log.
    pub fn with_history<R>(&self, f: impl FnOnce(&History) -> R) -> R {
        f(&self.lock())
    }
}

fn label_for(ops: &[Op]) -> String {
    match ops {
        [] => "nothing".into(),
        [one] => one.label(),
        [first, rest @ ..] => format!("{} and {} more", first.label(), rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cook::Cooker;
    use crate::job::JobPool;
    use crate::network::Port;
    use crate::testing::{create, test_types};
    use penumbra_core::{LatePolicy, ParamPath, Rate, ShowClock, Value};

    fn set(v: f64) -> Op {
        Op::Set { param: ParamPath::parse("/a.value").unwrap(), value: Value::Float(v) }
    }

    #[test]
    fn edits_publish_snapshots_and_undo() {
        let doc = Document::new(Arc::new(test_types()), Network::new());
        let g = doc.new_group();
        let out = doc.edit(&[create("/a", "test.const"), set(1.0)], Source::ControlChannel, g, "").unwrap();
        assert_eq!(out.recorded, Recorded::New);
        assert_eq!(out.created.len(), 1);
        assert!(Arc::ptr_eq(&doc.snapshot(), &out.snapshot));
        doc.with_history(|h| assert_eq!(h.entries()[0].label, "create /a (test.const) and 1 more"));

        let old = doc.snapshot();
        assert!(doc.edit(&[set(f64::NAN)], Source::Ui, doc.new_group(), "").is_err());
        assert!(Arc::ptr_eq(&doc.snapshot(), &old), "a failed edit publishes nothing");

        assert_eq!(doc.undo().as_deref(), Some("create /a (test.const) and 1 more"));
        assert!(doc.snapshot().is_empty());
        assert!(doc.redo().is_some());
        assert_eq!(doc.snapshot().len(), 1);
    }

    #[test]
    fn a_held_snapshot_is_unaffected_by_later_edits() {
        let doc = Document::new(Arc::new(test_types()), Network::new());
        doc.edit(&[create("/a", "test.const")], Source::Ui, doc.new_group(), "").unwrap();
        let held = doc.snapshot();
        doc.edit(&[set(5.0)], Source::Ui, doc.new_group(), "").unwrap();
        let a = held.resolve(&"/a".parse().unwrap()).unwrap();
        assert!(held.node(a).unwrap().params.is_empty());
    }

    #[test]
    fn cook_thread_reads_while_another_thread_edits() {
        let types = Arc::new(test_types());
        let doc = Arc::new(Document::new(types.clone(), Network::new()));
        doc.edit(&[create("/a", "test.const")], Source::Ui, doc.new_group(), "").unwrap();
        let a = doc.snapshot().resolve(&"/a".parse().unwrap()).unwrap();

        let writer = {
            let doc = doc.clone();
            std::thread::spawn(move || {
                let g = doc.new_group();
                for i in 1..=200 {
                    doc.edit(&[set(f64::from(i))], Source::Ui, g, "drag").unwrap();
                }
            })
        };
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        let mut last = 0.0;
        loop {
            let finished = writer.is_finished();
            cooker.cook(&doc.snapshot(), clock.now(), &[a], &[]);
            clock.advance(1);
            let v = cooker
                .output(Port { node: a, output: 0 })
                .unwrap()
                .get::<penumbra_core::Chans>()
                .unwrap()
                .value(None)
                .unwrap();
            assert!(v >= last, "values only grow: {v} after {last}");
            last = v;
            if finished {
                break;
            }
        }
        writer.join().unwrap();
        assert_eq!(last, 200.0);
        // One drag, one entry (plus the create).
        doc.with_history(|h| assert_eq!(h.entries().len(), 2));
    }
}
