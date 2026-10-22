//! The editor's side of copy and paste: through `penumbra_engine::clipboard`, as the user. The
//! copied text carries the nodes' positions, so the copies keep the arrangement of the originals,
//! moved to the pointer.

use std::collections::HashMap;
use std::time::Instant;

use egui::{Pos2, Rect, pos2, vec2};
use penumbra_core::{Error, NodeId, NodePath};
use penumbra_engine::Engine;
use penumbra_engine::clipboard as clip;
use penumbra_project::Actor;

use crate::editor::{Editor, level_path};
use crate::layout::{NODE_SIZE, place_ops};

/// What this run copied last.
pub struct Copied {
    pub text: String,
    /// How many nodes were copied (not counting what is inside them).
    pub count: usize,
}

fn err(e: penumbra_engine::CommandError) -> Error {
    Error::new(e.0)
}

fn paths(engine: &Engine, ids: &[NodeId]) -> Vec<NodePath> {
    let net = engine.session().doc().snapshot();
    ids.iter().filter_map(|id| net.path_of(*id)).collect()
}

/// Copies `ids` (side by side); the text for the system clipboard comes back in [`Copied`].
pub fn copy(engine: &Engine, ids: &[NodeId]) -> Result<Copied, Error> {
    let copied = clip::copy(engine, &paths(engine, ids)).map_err(err)?;
    Ok(Copied { count: copied.top.len(), text: copied.text })
}

/// Copies `ids`, then deletes them (one undo step).
pub fn cut(engine: &Engine, ids: &[NodeId]) -> Result<Copied, Error> {
    let (copied, _) = clip::cut(engine, Actor::User, &paths(engine, ids)).map_err(err)?;
    Ok(Copied { count: copied.top.len(), text: copied.text })
}

/// Pastes `text` (default: what this run copied last) inside `level`, the copies centred on
/// `at` in the arrangement the text gives them (what is inside them keeps its place on its own
/// plane), copies without a position side by side; one undo step. The copies become the
/// selection; returns a message when wires or bindings were left out.
pub fn paste(
    engine: &Engine,
    editor: &mut Editor,
    copied: Option<&Copied>,
    text: Option<&str>,
    level: Option<NodeId>,
    at: Pos2,
    now: Instant,
) -> Result<Option<String>, Error> {
    let text = text.or(copied.map(|c| c.text.as_str()));
    let net = engine.session().doc().snapshot();
    let into = level_path(&net, level);
    let pasted = clip::paste(engine, Actor::User, text, &into).map_err(err)?;
    let net = pasted.outcome.snapshot.clone();
    let top: Vec<NodeId> = pasted.top.iter().filter_map(|p| net.resolve(p)).collect();
    let pos_of = |id: &NodeId| net.node(*id).and_then(|n| n.pos).map(|[x, y]| pos2(x, y));
    let bounds =
        top.iter().filter_map(pos_of).map(|p| Rect::from_min_size(p, NODE_SIZE)).reduce(|a, b| a.union(b));
    let mut next = at - NODE_SIZE / 2.0;
    let mut to: HashMap<NodeId, Pos2> = HashMap::new();
    for id in &top {
        let p = match (pos_of(id), bounds) {
            (Some(p), Some(b)) => p + (at - b.center()),
            _ => {
                let p = next;
                next += vec2(NODE_SIZE.x + 40.0, 0.0);
                p
            }
        };
        to.insert(*id, p);
    }
    let ops = place_ops(&net, &to);
    if !ops.is_empty() {
        // Joins the paste's entry: one undo step for both.
        engine.session().edit(Actor::User, &ops, Some(pasted.outcome.group), "", now)?;
    }
    editor.select(top);
    if pasted.dropped.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("pasted {} node(s), without {}", pasted.top.len(), pasted.dropped.join(", "))))
}
